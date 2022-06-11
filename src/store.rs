/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write;
use std::iter::{repeat, IntoIterator};
use std::mem;
use std::os::raw::{c_int, c_uint, c_void};
use std::sync::Mutex;

use bstr::{BStr, BString, ByteSlice};
use derive_more::{Deref, Display};
use getset::{CopyGetters, Getters};
use itertools::Itertools;
use once_cell::sync::Lazy;
use percent_encoding::{percent_decode, percent_encode, NON_ALPHANUMERIC};

use crate::hg_data::{GitAuthorship, HgAuthorship, HgCommitter};
use crate::libc::FdFile;
use crate::libcinnabar::{generate_manifest, git2hg, hg2git, hg_object_id, send_buffer_to};
use crate::libgit::{
    get_oid_committish, lookup_replace_commit, object_id, object_type, strbuf, BlobId, Commit,
    CommitId, RawBlob, RawCommit,
};
use crate::oid::{GitObjectId, HgObjectId, ObjectId};
use crate::oid_type;
use crate::util::{FromBytes, ImmutBString, SliceExt, ToBoxed};
use crate::xdiff::{apply, textdiff, PatchInfo};

pub const REFS_PREFIX: &str = "refs/cinnabar/";
pub const REPLACE_REFS_PREFIX: &str = "refs/cinnabar/replace/";
pub const METADATA_REF: &str = "refs/cinnabar/metadata";
pub const CHECKED_REF: &str = "refs/cinnabar/checked";
pub const BROKEN_REF: &str = "refs/cinnabar/broken";
pub const NOTES_REF: &str = "refs/notes/cinnabar";

macro_rules! hg2git {
    ($h:ident => $g:ident($i:ident)) => {
        oid_type!($g($i));
        oid_type!($h(HgObjectId));

        impl $h {
            pub fn to_git(&self) -> Option<$g> {
                unsafe {
                    hg2git
                        .get_note(&self)
                        .map(|o| $g::from_unchecked($i::from_unchecked(o)))
                }
            }
        }
    };
}

hg2git!(HgChangesetId => GitChangesetId(CommitId));
hg2git!(HgManifestId => GitManifestId(CommitId));
hg2git!(HgFileId => GitFileId(BlobId));

oid_type!(GitChangesetMetadataId(BlobId));
oid_type!(GitFileMetadataId(BlobId));

impl GitChangesetId {
    pub fn to_hg(&self) -> Option<HgChangesetId> {
        //TODO: avoid repeatedly reading metadata for a given changeset.
        //The equivalent python code was keeping a LRU cache.
        let metadata = RawGitChangesetMetadata::read(self);
        metadata
            .as_ref()
            .and_then(RawGitChangesetMetadata::parse)
            .map(|m| m.changeset_id().clone())
    }
}

pub struct RawGitChangesetMetadata(RawBlob);

impl RawGitChangesetMetadata {
    pub fn read(changeset_id: &GitChangesetId) -> Option<Self> {
        let note = unsafe {
            git2hg
                .get_note(changeset_id)
                .map(|o| BlobId::from_unchecked(o))?
        };
        RawBlob::read(&note).map(Self)
    }

    pub fn parse(&self) -> Option<ParsedGitChangesetMetadata> {
        let mut changeset = None;
        let mut manifest = None;
        let mut author = None;
        let mut extra = None;
        let mut files = None;
        let mut patch = None;
        for line in self.0.as_bytes().lines() {
            match line.splitn_exact(b' ')? {
                [b"changeset", c] => changeset = Some(HgChangesetId::from_bytes(c).ok()?),
                [b"manifest", m] => manifest = Some(HgManifestId::from_bytes(m).ok()?),
                [b"author", a] => author = Some(a),
                [b"extra", e] => extra = Some(e),
                [b"files", f] => files = Some(f),
                [b"patch", p] => patch = Some(p),
                _ => None?,
            }
        }

        Some(ParsedGitChangesetMetadata {
            changeset_id: changeset?,
            manifest_id: manifest.unwrap_or_else(HgManifestId::null),
            author,
            extra,
            files,
            patch,
        })
    }
}

#[derive(CopyGetters, Getters)]
pub struct GitChangesetMetadata<B: AsRef<[u8]>> {
    #[getset(get = "pub")]
    changeset_id: HgChangesetId,
    #[getset(get = "pub")]
    manifest_id: HgManifestId,
    author: Option<B>,
    extra: Option<B>,
    files: Option<B>,
    patch: Option<B>,
}

pub type ParsedGitChangesetMetadata<'a> = GitChangesetMetadata<&'a [u8]>;

impl<B: AsRef<[u8]>> GitChangesetMetadata<B> {
    pub fn author(&self) -> Option<&[u8]> {
        self.author.as_ref().map(B::as_ref)
    }

    pub fn extra(&self) -> Option<ChangesetExtra> {
        self.extra
            .as_ref()
            .map(|b| ChangesetExtra::from(b.as_ref()))
    }

    pub fn files(&self) -> impl Iterator<Item = &[u8]> {
        let mut split = self
            .files
            .as_ref()
            .map_or(&b""[..], B::as_ref)
            .split(|&b| b == b'\0');
        if self.files.is_none() {
            // b"".split() would return an empty first item, and we want to skip that.
            split.next();
        }
        split
    }

    pub fn patch(&self) -> Option<GitChangesetPatch> {
        self.patch.as_ref().map(|b| GitChangesetPatch(b.as_ref()))
    }
}

pub type GeneratedGitChangesetMetadata = GitChangesetMetadata<ImmutBString>;

impl GeneratedGitChangesetMetadata {
    pub fn generate(
        commit: &Commit,
        changeset_id: &HgChangesetId,
        raw_changeset: &RawHgChangeset,
    ) -> Option<Self> {
        let changeset = raw_changeset.parse()?;
        let changeset_id = changeset_id.clone();
        let manifest_id = changeset.manifest().clone();
        let author = if commit.author() != changeset.author() {
            Some(changeset.author().to_vec().into_boxed_slice())
        } else {
            None
        };
        let extra = changeset.extra.map(|b| b.to_vec().into_boxed_slice());
        let files = changeset.files.map(|b| b.to_vec().into_boxed_slice());
        let mut temp = GeneratedGitChangesetMetadata {
            changeset_id,
            manifest_id,
            author,
            extra,
            files,
            patch: None,
        };
        let new = RawHgChangeset::from_metadata(commit, &temp)?;
        if **raw_changeset != *new {
            // TODO: produce a better patch (byte_diff)
            temp.patch = Some(GitChangesetPatch::from_patch_info(textdiff(
                raw_changeset,
                &new,
            )));
        }
        Some(temp)
    }
}

pub struct ChangesetExtra<'a> {
    data: BTreeMap<&'a BStr, &'a BStr>,
}

impl<'a> ChangesetExtra<'a> {
    fn from(buf: &'a [u8]) -> Self {
        if buf.is_empty() {
            ChangesetExtra::new()
        } else {
            ChangesetExtra {
                data: buf
                    .split(|&c| c == b'\0')
                    .map(|a| {
                        let [k, v] = a.splitn_exact(b':').unwrap();
                        (k.as_bstr(), v.as_bstr())
                    })
                    .collect(),
            }
        }
    }

    pub fn new() -> Self {
        ChangesetExtra {
            data: BTreeMap::new(),
        }
    }

    pub fn get(&self, name: &'a [u8]) -> Option<&'a [u8]> {
        self.data.get(name.as_bstr()).map(|b| &***b)
    }

    pub fn set(&mut self, name: &'a [u8], value: &'a [u8]) {
        self.data.insert(name.as_bstr(), value.as_bstr());
    }

    pub fn dump_into(&self, buf: &mut Vec<u8>) {
        for b in Itertools::intersperse(
            self.data.iter().map(|(&k, &v)| {
                let mut buf = Vec::new();
                buf.extend_from_slice(k);
                buf.push(b':');
                buf.extend_from_slice(v);
                Cow::Owned(buf)
            }),
            Cow::Borrowed(&b"\0"[..]),
        ) {
            buf.extend_from_slice(&b);
        }
    }
}

#[test]
fn test_changeset_extra() {
    let mut extra = ChangesetExtra::new();
    extra.set(b"foo", b"bar");
    extra.set(b"bar", b"qux");
    let mut result = Vec::new();
    extra.dump_into(&mut result);
    assert_eq!(result.as_bstr(), b"bar:qux\0foo:bar".as_bstr());

    let mut extra = ChangesetExtra::from(&result);
    let mut result2 = Vec::new();
    extra.dump_into(&mut result2);
    assert_eq!(result.as_bstr(), result2.as_bstr());

    extra.set(b"aaaa", b"bbbb");
    result2.truncate(0);
    extra.dump_into(&mut result2);
    assert_eq!(result2.as_bstr(), b"aaaa:bbbb\0bar:qux\0foo:bar".as_bstr());
}

pub struct GitChangesetPatch<'a>(&'a [u8]);

impl<'a> GitChangesetPatch<'a> {
    pub fn iter(&self) -> Option<impl Iterator<Item = PatchInfo<Cow<'a, [u8]>>>> {
        self.0
            .split(|c| *c == b'\0')
            .map(|part| {
                let [start, end, data] = part.splitn_exact(b',')?;
                let start = usize::from_bytes(start).ok()?;
                let end = usize::from_bytes(end).ok()?;
                let data = Cow::from(percent_decode(data));
                Some(PatchInfo { start, end, data })
            })
            .collect::<Option<Vec<_>>>()
            .map(IntoIterator::into_iter)
    }

    pub fn apply(&self, input: &[u8]) -> Option<ImmutBString> {
        Some(apply(self.iter()?, input))
    }

    pub fn from_patch_info(
        iter: impl Iterator<Item = PatchInfo<impl AsRef<[u8]>>>,
    ) -> ImmutBString {
        let mut result = Vec::new();
        for (n, part) in iter.enumerate() {
            if n > 0 {
                result.push(b'\0');
            }
            write!(
                result,
                "{},{},{}",
                part.start,
                part.end,
                percent_encode(part.data.as_ref(), NON_ALPHANUMERIC)
            )
            .ok();
        }
        result.into_boxed_slice()
    }
}

#[derive(Deref)]
#[deref(forward)]
pub struct RawHgChangeset(pub ImmutBString);

impl RawHgChangeset {
    pub fn from_metadata<B: AsRef<[u8]>>(
        commit: &Commit,
        metadata: &GitChangesetMetadata<B>,
    ) -> Option<Self> {
        let HgAuthorship {
            author: mut hg_author,
            timestamp: hg_timestamp,
            utcoffset: hg_utcoffset,
        } = GitAuthorship(commit.author()).into();
        let hg_committer = if commit.author() != commit.committer() {
            Some(HgCommitter::from(GitAuthorship(commit.committer())).0)
        } else {
            None
        };
        let hg_committer = hg_committer.as_ref();

        if let Some(author) = metadata.author() {
            hg_author = author.to_boxed();
        }
        let mut extra = metadata.extra();
        if let Some(hg_committer) = hg_committer {
            extra
                .get_or_insert_with(ChangesetExtra::new)
                .set(b"committer", hg_committer);
        };
        let mut changeset = Vec::new();
        writeln!(changeset, "{}", metadata.manifest_id()).ok()?;
        changeset.extend_from_slice(&hg_author);
        changeset.push(b'\n');
        changeset.extend_from_slice(&hg_timestamp);
        changeset.push(b' ');
        changeset.extend_from_slice(&hg_utcoffset);
        if let Some(extra) = extra {
            changeset.push(b' ');
            extra.dump_into(&mut changeset);
        }
        let mut files = metadata.files().collect_vec();
        //TODO: probably don't actually need sorting.
        files.sort();
        for f in &files {
            changeset.push(b'\n');
            changeset.extend_from_slice(f);
        }
        changeset.extend_from_slice(b"\n\n");
        changeset.extend_from_slice(commit.body());

        if let Some(patch) = metadata.patch() {
            let mut patched = patch.apply(&changeset)?.to_vec();
            mem::swap(&mut changeset, &mut patched);
        }

        // Adjust for `handle_changeset_conflict`.
        // TODO: when creating the git2hg metadata moves to Rust, we can
        // create a patch instead, which would be handled above instead of
        // manually here.
        let node = metadata.changeset_id();
        while changeset[changeset.len() - 1] == b'\0' {
            let mut hash = HgChangesetId::create();
            let mut parents = commit
                .parents()
                .iter()
                .map(|p| unsafe { GitChangesetId::from_unchecked(p.clone()) }.to_hg())
                .collect::<Option<Vec<_>>>()?;
            parents.sort();
            for p in parents.iter().chain(repeat(&HgChangesetId::null())).take(2) {
                hash.update(p.as_raw_bytes());
            }
            hash.update(&changeset);
            if hash.finalize() == *node {
                break;
            }
            changeset.pop();
        }
        Some(RawHgChangeset(changeset.into()))
    }

    pub fn read(oid: &GitChangesetId) -> Option<Self> {
        let commit = RawCommit::read(oid)?;
        let commit = commit.parse()?;
        let metadata = RawGitChangesetMetadata::read(oid)?;
        let metadata = metadata.parse()?;
        Self::from_metadata(&commit, &metadata)
    }

    pub fn parse(&self) -> Option<HgChangeset> {
        let [header, body] = self.0.splitn_exact(&b"\n\n"[..])?;
        let mut lines = header.splitn(4, |&b| b == b'\n');
        let manifest = lines.next()?;
        let author = lines.next()?;
        let mut date = lines.next()?.splitn(3, |&b| b == b' ');
        let timestamp = date.next()?;
        let utcoffset = date.next()?;
        let extra = date.next();
        let files = lines.next();
        Some(HgChangeset {
            manifest: HgManifestId::from_bytes(manifest).ok()?,
            author,
            timestamp,
            utcoffset,
            extra,
            files,
            body,
        })
    }
}

#[derive(CopyGetters, Getters)]
pub struct HgChangeset<'a> {
    #[getset(get = "pub")]
    manifest: HgManifestId,
    #[getset(get_copy = "pub")]
    author: &'a [u8],
    #[getset(get_copy = "pub")]
    timestamp: &'a [u8],
    #[getset(get_copy = "pub")]
    utcoffset: &'a [u8],
    extra: Option<&'a [u8]>,
    files: Option<&'a [u8]>,
    #[getset(get_copy = "pub")]
    body: &'a [u8],
}

impl<'a> HgChangeset<'a> {
    pub fn extra(&self) -> Option<ChangesetExtra> {
        self.extra.map(ChangesetExtra::from)
    }

    pub fn files(&self) -> impl Iterator<Item = &[u8]> {
        let mut split = self.files.unwrap_or(b"").split(|&b| b == b'\n');
        if self.files.is_none() {
            // b"".split() would return an empty first item, and we want to skip that.
            split.next();
        }
        split
    }
}

#[derive(Deref)]
#[deref(forward)]
pub struct RawHgManifest(ImmutBString);

impl RawHgManifest {
    pub fn read(oid: &GitManifestId) -> Option<Self> {
        unsafe {
            generate_manifest(&(&***oid).clone().into())
                .as_ref()
                .map(|b| Self(b.as_bytes().to_owned().into()))
        }
    }
}

#[derive(Deref)]
#[deref(forward)]
pub struct RawHgFile(ImmutBString);

impl RawHgFile {
    pub fn read(oid: &GitFileId, metadata: Option<&GitFileMetadataId>) -> Option<Self> {
        let mut result = Vec::new();
        if let Some(metadata) = metadata {
            result.extend_from_slice(b"\x01\n");
            result.extend_from_slice(RawBlob::read(metadata)?.as_bytes());
            result.extend_from_slice(b"\x01\n");
        }
        result.extend_from_slice(RawBlob::read(oid)?.as_bytes());
        Some(Self(result.into()))
    }
}

#[derive(Debug)]
struct ChangesetHeads {
    generation: usize,
    heads: BTreeMap<HgChangesetId, (BString, usize)>,
}

impl ChangesetHeads {
    fn new() -> Self {
        get_oid_committish(b"refs/cinnabar/metadata^1").map_or_else(
            || ChangesetHeads {
                generation: 0,
                heads: BTreeMap::new(),
            },
            |cid| {
                let commit = RawCommit::read(&cid).unwrap();
                let commit = commit.parse().unwrap();
                let heads = commit
                    .body()
                    .lines()
                    .enumerate()
                    .map(|(n, l)| {
                        let [h, b] = l.splitn_exact(b' ').unwrap();
                        (HgChangesetId::from_bytes(h).unwrap(), (BString::from(b), n))
                    })
                    .collect::<BTreeMap<_, _>>();
                ChangesetHeads {
                    generation: heads.len(),
                    heads,
                }
            },
        )
    }
}

static CHANGESET_HEADS: Lazy<Mutex<ChangesetHeads>> =
    Lazy::new(|| Mutex::new(ChangesetHeads::new()));

#[no_mangle]
pub unsafe extern "C" fn add_changeset_head(cs: *const hg_object_id, oid: *const object_id) {
    let cs = HgChangesetId::from_unchecked(HgObjectId::from(cs.as_ref().unwrap().clone()));

    // Because we don't keep track of many of these things in the rust side right now,
    // we do extra work here. Eventually, this will be simplified.
    let mut heads = CHANGESET_HEADS.lock().unwrap();
    let oid = GitObjectId::from(oid.as_ref().unwrap().clone());
    if oid == GitObjectId::null() {
        heads.heads.remove(&cs);
    } else {
        let blob = BlobId::from_unchecked(oid);
        let cs_meta = RawGitChangesetMetadata(RawBlob::read(&blob).unwrap());
        let meta = cs_meta.parse().unwrap();
        assert_eq!(meta.changeset_id, cs);
        let branch = meta
            .extra()
            .and_then(|e| e.get(b"branch"))
            .unwrap_or(b"default");
        let cid = cs.to_git().unwrap();
        let commit = RawCommit::read(&cid).unwrap();
        let commit = commit.parse().unwrap();
        for parent in commit.parents() {
            let parent = lookup_replace_commit(parent);
            let parent_cs_meta =
                RawGitChangesetMetadata::read(&GitChangesetId::from_unchecked(parent.into_owned()))
                    .unwrap();
            let parent_meta = parent_cs_meta.parse().unwrap();
            let parent_branch = parent_meta
                .extra()
                .and_then(|e| e.get(b"branch"))
                .unwrap_or(b"default");
            if parent_branch == branch {
                if let Some((b, _)) = heads.heads.get(&parent_meta.changeset_id) {
                    assert_eq!(b.as_bstr(), parent_branch.as_bstr());
                    heads.heads.remove(&parent_meta.changeset_id);
                }
            }
        }
        let generation = heads.generation;
        heads.generation += 1;
        heads.heads.insert(cs, (BString::from(branch), generation));
    }
}

#[no_mangle]
pub unsafe extern "C" fn changeset_heads(output: c_int) {
    let mut output = FdFile::from_raw_fd(output);
    let heads = CHANGESET_HEADS.lock().unwrap();

    let mut buf = Vec::new();
    for (_, h, b) in heads.heads.iter().map(|(h, (b, g))| (g, h, b)).sorted() {
        writeln!(buf, "{} {}", h, b).ok();
    }
    send_buffer_to(&*buf, &mut output);
}

extern "C" {
    fn write_object_file_flags(
        buf: *const c_void,
        len: usize,
        typ: object_type,
        oid: *mut object_id,
        flags: c_uint,
    ) -> c_int;
}

#[no_mangle]
pub unsafe extern "C" fn store_changesets_metadata(blob: *const object_id, result: *mut object_id) {
    let result = result.as_mut().unwrap();
    let mut tree = vec![];
    if let Some(blob) = blob.as_ref() {
        let blob = BlobId::from_unchecked(GitObjectId::from(blob.clone()));
        tree.extend_from_slice(b"100644 bundle\0");
        tree.extend_from_slice(blob.as_raw_bytes());
    }
    let mut tid = object_id::default();
    write_object_file_flags(
        tree.as_ptr() as *const c_void,
        tree.len(),
        object_type::OBJ_TREE,
        &mut tid,
        0,
    );
    drop(tree);
    let mut commit = vec![];
    writeln!(commit, "tree {}", GitObjectId::from(tid)).ok();
    let heads = CHANGESET_HEADS.lock().unwrap();
    for (_, head) in heads.heads.iter().map(|(h, (_, g))| (g, h)).sorted() {
        writeln!(commit, "parent {}", head.to_git().unwrap()).ok();
    }
    writeln!(commit, "author  <cinnabar@git> 0 +0000").ok();
    writeln!(commit, "committer  <cinnabar@git> 0 +0000").ok();
    for (_, head, branch) in heads.heads.iter().map(|(h, (b, g))| (g, h, b)).sorted() {
        write!(commit, "\n{} {}", head, branch).ok();
    }
    write_object_file_flags(
        commit.as_ptr() as *const c_void,
        commit.len(),
        object_type::OBJ_COMMIT,
        result,
        0,
    );
}

#[no_mangle]
pub unsafe extern "C" fn reset_changeset_heads() {
    let mut heads = CHANGESET_HEADS.lock().unwrap();
    *heads = ChangesetHeads::new();
}

#[no_mangle]
pub unsafe extern "C" fn prepare_changeset_commit(
    changeset_id: *const hg_object_id,
    tree_id: *const object_id,
    parent1: *const hg_object_id,
    parent2: *const hg_object_id,
    changeset_buf: *const strbuf,
    commit_buf: *mut strbuf,
) {
    let _changeset_id =
        HgChangesetId::from_unchecked(HgObjectId::from(changeset_id.as_ref().unwrap().clone()));
    let tree_id = GitObjectId::from(tree_id.as_ref().unwrap().clone());
    let parent1 = parent1
        .as_ref()
        .cloned()
        .map(|p| HgChangesetId::from_unchecked(HgObjectId::from(p)));
    let parent2 = parent2
        .as_ref()
        .cloned()
        .map(|p| HgChangesetId::from_unchecked(HgObjectId::from(p)));
    let changeset = RawHgChangeset(changeset_buf.as_ref().unwrap().as_bytes().into());
    let changeset = changeset.parse().unwrap();
    let result = commit_buf.as_mut().unwrap();
    let author = HgAuthorship {
        author: changeset.author(),
        timestamp: changeset.timestamp(),
        utcoffset: changeset.utcoffset(),
    };
    // TODO: reduce the amount of cloning.
    let git_author = GitAuthorship::from(author.clone());
    let git_committer = if let Some(extra) = changeset.extra() {
        if let Some(committer) = extra.get(b"committer") {
            if committer.ends_with(b">") {
                GitAuthorship::from(HgAuthorship {
                    author: committer,
                    timestamp: author.timestamp,
                    utcoffset: author.utcoffset,
                })
            } else {
                GitAuthorship::from(HgCommitter(committer))
            }
        } else {
            git_author.clone()
        }
    } else {
        git_author.clone()
    };
    result.extend_from_slice(format!("tree {}\n", tree_id).as_bytes());
    if let Some(parent1) = parent1 {
        result.extend_from_slice(format!("parent {}\n", parent1.to_git().unwrap()).as_bytes());
    }
    if let Some(parent2) = parent2 {
        result.extend_from_slice(format!("parent {}\n", parent2.to_git().unwrap()).as_bytes());
    }
    result.extend_from_slice(b"author ");
    result.extend_from_slice(&git_author.0);
    result.extend_from_slice(b"\ncommitter ");
    result.extend_from_slice(&git_committer.0);
    result.extend_from_slice(b"\n\n");
    result.extend_from_slice(changeset.body());
}
