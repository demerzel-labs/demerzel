"""The shared mincore probe. Both platform modules call it with their
own libc handle; the syscall name is the only native part, so the page
math and the mapping setup live once."""

import ctypes
import mmap
import os


def resident_pages(libc, path: str) -> int:
    """Pages of `path` in the page cache, via mincore. The mapping is
    read-only and unmapped before return, so it does not pin pages."""
    size = os.path.getsize(path)
    fd = os.open(path, os.O_RDONLY)
    try:
        # MAP_PRIVATE read-write: ctypes.from_buffer needs a writable
        # buffer to take the address. Nothing writes to the mapping,
        # so no page is copied and mincore sees the file pages.
        mapped = mmap.mmap(
            fd, size, flags=mmap.MAP_PRIVATE, prot=mmap.PROT_READ | mmap.PROT_WRITE
        )
    finally:
        os.close(fd)
    try:
        page = os.sysconf("SC_PAGE_SIZE")
        pages = (size + page - 1) // page
        vec = (ctypes.c_ubyte * pages)()
        buf = (ctypes.c_char * size).from_buffer(mapped)
        rc = libc.mincore(
            ctypes.c_void_p(ctypes.addressof(buf)), ctypes.c_size_t(size), vec
        )
        del buf
        if rc != 0:
            raise OSError(ctypes.get_errno(), "mincore failed")
        return sum(v & 1 for v in vec)
    finally:
        mapped.close()
