# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.

import hashlib
import json
import os

from tasks import (
    Task,
    TaskEnvironment,
    bash_command,
    join_command,
    no_quote,
)
from variables import TC_REPO_NAME


def sources_list(snapshot, sections):
    for idx, (archive, dist) in enumerate(sections):
        if not snapshot:
            yield 'deb http://archive.debian.org/{} {} main'.format(
                archive,
                dist,
            )
            continue
        yield 'deb http://snapshot.debian.org/archive/{}/{} {} main'.format(
            archive,
            snapshot,
            dist,
        )


DOCKER_IMAGES = {
    'base': {
        'from': 'debian:stretch-20220622',
        'commands': [
            '({}) > /etc/apt/sources.list'.format('; '.join(
                'echo ' + l for l in sources_list(None, (
                    ('debian', 'stretch'),
                    ('debian', 'stretch-proposed-updates'),
                    ('debian-security', 'stretch/updates'),
                )))),
            'apt-get update -o Acquire::Check-Valid-Until=false',
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'apt-transport-https',
                'bzip2',
                'ca-certificates',
                'curl',
                'gnupg2',
                'libcurl3-gnutls',
                'python-setuptools',
                'python-pip',
                'python3-setuptools',
                'python3-pip',
                'unzip',
                'xz-utils',
                'zip',
                'zstd',
            ])),
            'apt-get clean',
            'curl -sO https://apt.llvm.org/llvm-snapshot.gpg.key',
            'gpg --no-default-keyring --keyring /usr/share/keyrings/llvm.gpg'
            ' --import llvm-snapshot.gpg.key',
            'rm llvm-snapshot.gpg.key',
            'echo'
            ' deb [signed-by=/usr/share/keyrings/llvm.gpg]'
            ' https://apt.llvm.org/stretch/ llvm-toolchain-stretch-15 main'
            ' > /etc/apt/sources.list.d/llvm.list',
            'apt-get update -o Acquire::Check-Valid-Until=false',
            'python2.7 -m pip install pip==20.3.4 wheel==0.37.1'
            ' --upgrade --ignore-installed',
            'python3 -m pip install pip==20.3.4 wheel==0.37.1'
            ' --upgrade --ignore-installed',
        ],
    },

    'base-buster': {
        'from': 'debian:buster-20220801',
        'commands': [
            '({}) > /etc/apt/sources.list'.format('; '.join(
                'echo ' + l for l in sources_list('20220801T205040Z', (
                    ('debian', 'buster'),
                    ('debian', 'buster-updates'),
                    ('debian-security', 'buster/updates'),
                )))),
            'apt-get update -o Acquire::Check-Valid-Until=false',
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'apt-transport-https',
                'bzip2',
                'ca-certificates',
                'curl',
                'gnupg2',
                'libcurl3-gnutls',
                'python-setuptools',
                'python-pip',
                'python3-setuptools',
                'python3-pip',
                'unzip',
                'xz-utils',
                'zip',
                'zstd',
            ])),
            'apt-get clean',
            'curl -sO https://apt.llvm.org/llvm-snapshot.gpg.key',
            'gpg --no-default-keyring --keyring /usr/share/keyrings/llvm.gpg'
            ' --import llvm-snapshot.gpg.key',
            'rm llvm-snapshot.gpg.key',
            'echo'
            ' deb [signed-by=/usr/share/keyrings/llvm.gpg]'
            ' https://apt.llvm.org/buster/ llvm-toolchain-buster-15 main'
            ' > /etc/apt/sources.list.d/llvm.list',
            'apt-get update -o Acquire::Check-Valid-Until=false',
            'python2.7 -m pip install pip==20.3.4 wheel==0.37.1'
            ' --upgrade --ignore-installed',
            'python3 -m pip install pip==20.3.4 wheel==0.37.1'
            ' --upgrade --ignore-installed',
        ],
    },

    'build': {
        'from': 'base',
        'commands': [
            'dpkg --add-architecture arm64',
            'apt-get update -o Acquire::Check-Valid-Until=false',
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'clang-15',
                'gcc',
                'git',
                'make',
                'patch',
                'pkg-config',
                'python-dev',
                'python3-dev',
                'libc6-dev',
                'libcurl4-gnutls-dev',
                'zlib1g-dev',
            ])),
            'echo path-exclude=/usr/bin/curl-config'
            ' > /etc/dpkg/dpkg.cfg.d/excludes',
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'libc6-dev:arm64',
                'libcurl4-gnutls-dev:arm64',
                'zlib1g-dev:arm64',
                'libgcc-6-dev:arm64',
                'binutils-aarch64-linux-gnu',
                'dpkg-dev',
            ])),
            'apt-get clean',
            'ln -s /usr/bin/aarch64-linux-gnu-ld'
            ' /usr/bin/aarch64-unknown-linux-gnu-ld',
        ],
    },

    'build-buster': {
        'from': 'base-buster',
        'commands': [
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'gcc',
                'git',
                'libc6-dev',
                'make',
                'patch',
                'python-dev',
                'python3-dev',
                'zlib1g-dev',
            ])),
            'apt-get clean',
        ],
    },

    'codecov': {
        'from': 'base',
        'commands': [
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'gcc',
                'git',
                'python3-coverage',
            ])),
            'apt-get clean',
            'ln -s /usr/bin/python3-coverage /usr/local/bin/coverage',
            'curl -o /usr/local/bin/codecov -sL {}'.format(
                'https://github.com/codecov/uploader/releases/download'
                '/v0.1.0_9779/codecov-linux'
            ),
            'chmod +x /usr/local/bin/codecov',
            'curl -sL {} | tar -C /usr/local/bin -jxf -'.format(
                'https://github.com/mozilla/grcov/releases/download/v0.8.7'
                '/grcov-x86_64-unknown-linux-gnu.tar.bz2'
            ),
        ],
    },

    'test': {
        'from': 'base-buster',
        'commands': [
            'apt-get install -y --no-install-recommends {}'.format(' '.join([
                'llvm-15',
                'make',
            ])),
            'apt-get clean',
            'pip3 install cram==0.7',
            'ln -s llvm-symbolizer-15 /usr/bin/llvm-symbolizer'
        ],
    },
}


class DockerImage(Task, metaclass=TaskEnvironment):
    PREFIX = 'linux'
    cpu = 'x86_64'
    os = 'linux'

    def __init__(self, name):
        defn = DOCKER_IMAGES[name]
        base = defn['from']
        self.name = name
        if ':' not in base:
            base = DockerImage.by_name(base)
        self.base = base
        self.definition = defn['commands']

        Task.__init__(
            self,
            task_env=self,
            description='docker image: {}'.format(name),
            index=self.index,
            expireIn='26 weeks',
            workerType='linux',
            image=base,
            dockerSave=True,
            command=self.definition,
        )

    def __str__(self):
        return '{}/{}:{}'.format(
            TC_REPO_NAME,
            self.name,
            self.hexdigest
        )

    @property
    def index(self):
        return '.'.join((self.PREFIX, self.name, self.hexdigest))

    @property
    def hexdigest(self):
        h = hashlib.sha1()
        h.update(str(self.base).encode())
        h.update(json.dumps(self.definition).encode())
        return h.hexdigest()

    def prepare_params(self, params):
        commands = ["mkdir artifacts"]
        image = params.pop('image', self)
        volumes = [
            kind.split(':', 1)[1]
            for mount in params.get('mounts', [])
            for kind in mount
            if ':' in kind
        ]
        if isinstance(image, DockerImage):
            params.setdefault('mounts', []).append({'file': image})
            artifact = os.path.basename(image.artifact)
            commands.append(
                f'IMAGE_NAME=$(zstd -cd {artifact} | podman load'
                ' | sed -n "s/.*: //p")')
            image = no_quote('$IMAGE_NAME')
        run_cmd = [
            'podman',
            'run',
            '--name=taskcontainer',
            '--volume=./artifacts:/artifacts',
            '--env=ARTIFACTS=/artifacts',
        ]
        if any(s.startswith('secrets:') for s in params.get('scopes', [])):
            # There's probably a better way, but it's simpler.
            run_cmd.append('--network=host')
        for v in volumes:
            run_cmd.append(f'--volume=./{v}:/{v}')
        for k, v in params.pop('env', {}).items():
            run_cmd.append(f'--env={k}={v}')
        for cap in params.pop('caps', []):
            run_cmd.append(f'--cap-add={cap}')
        run_cmd.append(image)
        run_cmd.extend(bash_command(*params['command']))
        commands.append(join_command(*run_cmd))
        if params.pop('dockerSave', False):
            commands.extend([
                'exit_code=$?',
                'podman commit taskcontainer taskcontainer',
                'podman save taskcontainer'
                ' | zstd > artifacts/dockerImage.tar.zst',
                'podman rm taskcontainer',
                'exit $exit_code',
            ])
            params['artifacts'] = ["dockerImage.tar.zst"]
        params['command'] = bash_command(*commands)

        if 'artifacts' in params:
            params['artifacts'] = [f'artifacts/{a}'
                                   for a in params['artifacts']]
        return params
