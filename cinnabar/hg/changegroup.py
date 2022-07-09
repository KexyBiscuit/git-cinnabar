from binascii import (
    hexlify,
    unhexlify,
)
from cinnabar.git import NULL_NODE_ID


class ParentsTrait(object):
    __slots__ = ()

    @property
    def parents(self):
        if self.parent1 != NULL_NODE_ID:
            if self.parent2 != NULL_NODE_ID:
                return (self.parent1, self.parent2)
            return (self.parent1,)
        if self.parent2 != NULL_NODE_ID:
            return (self.parent2,)
        return ()

    @parents.setter
    def parents(self, parents):
        assert isinstance(parents, (tuple, list))
        assert len(parents) <= 2
        if len(parents):
            self.parent1 = parents[0]
        if len(parents) > 1:
            self.parent2 = parents[1]
        else:
            self.parent2 = NULL_NODE_ID
        if not parents:
            self.parent1 = NULL_NODE_ID


class RawRevChunk(bytearray, ParentsTrait):
    __slots__ = ()

    @staticmethod
    def _field(offset, size=None, filter=bytes):
        unfilter = unhexlify if filter == hexlify else None
        end = offset + size if size else None

        class descriptor(object):
            def __get__(self, obj, type=None):
                return filter(obj[offset:end])

            def __set__(self, obj, value):
                value = unfilter(value) if unfilter else value
                assert len(value) == size or not size
                self.ensure(obj, end or offset)
                obj[offset:end] = value

            def ensure(self, obj, length):
                if length > len(obj):
                    obj.extend(b'\0' * (length - len(obj)))

        return descriptor()


class RawRevChunk01(RawRevChunk):
    __slots__ = ('__weakref__',)

    node = RawRevChunk._field(0, 20, hexlify)
    parent1 = RawRevChunk._field(20, 20, hexlify)
    parent2 = RawRevChunk._field(40, 20, hexlify)
    changeset = RawRevChunk._field(60, 20, hexlify)
    data = RawRevChunk._field(80)
    patch = RawRevChunk._field(80)

    # Because we keep so many instances of this class on hold, the overhead
    # of having a __dict__ per instance is a deal breaker.
    _delta_nodes = {}

    @property
    def delta_node(self):
        return self._delta_nodes.get(self.node, NULL_NODE_ID)

    @delta_node.setter
    def delta_node(self, value):
        self._delta_nodes[self.node] = value


class RawRevChunk02(RawRevChunk):
    __slots__ = ()

    node = RawRevChunk._field(0, 20, hexlify)
    parent1 = RawRevChunk._field(20, 20, hexlify)
    parent2 = RawRevChunk._field(40, 20, hexlify)
    delta_node = RawRevChunk._field(60, 20, hexlify)
    changeset = RawRevChunk._field(80, 20, hexlify)
    data = RawRevChunk._field(100)
    patch = RawRevChunk._field(100)
