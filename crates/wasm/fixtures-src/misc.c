typedef unsigned int u32;
__attribute__((import_module("wasi_snapshot_preview1"), import_name("fd_write")))
int fd_write(int fd, const void *iovs, int n, int *nwritten);
__attribute__((import_module("wasi_snapshot_preview1"), import_name("proc_exit")))
void proc_exit(int code);
#ifdef UNKNOWN
__attribute__((import_module("wasi_snapshot_preview1"), import_name("path_open")))
int path_open(int a, int b, const char *p, int c, long long d, long long e, int f, int *g);
#endif
struct iov { const void *buf; u32 len; };
static void put(int fd, const char *s) { u32 n = 0; while (s[n]) n++; struct iov v = { s, n }; int w = 0; fd_write(fd, &v, 1, &w); }
static int putint(int fd, int x) { char b[2] = { (char)('0' + (x % 10)), 0 }; struct iov v = { b, 1 }; int w = 0; return fd_write(fd, &v, 1, &w); }

__attribute__((export_name("_start")))
void start(void) {
#if defined(LOOP)
    volatile unsigned long i = 0; for (;;) { i++; }
#elif defined(GROW)
    int r = __builtin_wasm_memory_grow(0, 100000);   /* ~6.5 GB */
    put(1, r == -1 ? "LOG grow refused\n" : "LOG grow allowed\n"); put(1, "DONE\n");
#elif defined(FD3)
    int w = 0; struct iov v = { "x", 1 };
    int e = fd_write(3, &v, 1, &w);
    put(1, "LOG fd3 errno "); putint(1, e); put(1, "\nDONE\n");
#elif defined(EXIT3)
    put(2, "boom on stderr\n"); proc_exit(3);
#elif defined(UNKNOWN)
    int x = 0; path_open(3, 0, "/etc/passwd", 0, 0, 0, 0, &x); put(1, "DONE\n");
#elif defined(FLOOD)
    static char big[65536]; for (u32 i = 0; i < sizeof big; i++) big[i] = 'x';
    struct iov v = { big, sizeof big }; int w = 0;
    for (;;) { if (fd_write(1, &v, 1, &w) != 0) break; }
    put(1, "DONE\n");
#endif
}
