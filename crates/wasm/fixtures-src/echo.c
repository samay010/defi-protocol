typedef unsigned int u32;
__attribute__((import_module("wasi_snapshot_preview1"), import_name("fd_read")))
int fd_read(int fd, const void *iovs, int n, int *nread);
__attribute__((import_module("wasi_snapshot_preview1"), import_name("fd_write")))
int fd_write(int fd, const void *iovs, int n, int *nwritten);
__attribute__((import_module("wasi_snapshot_preview1"), import_name("proc_exit")))
void proc_exit(int code);

struct iov { const void *buf; u32 len; };
static char in[65536];
static const char out[] = "LOG hello from wasm\nSETSTATE seen 1\nDONE\n";

static void write_all(int fd, const char *s, u32 len) {
    struct iov v = { s, len }; int n = 0;
    fd_write(fd, &v, 1, &n);
}

__attribute__((export_name("_start")))
void start(void) {
    struct iov v = { in, sizeof in }; int n = 0;
    u32 total = 0;
    for (;;) {
        v.buf = in + total; v.len = sizeof in - total;
        if (fd_read(0, &v, 1, &n) != 0 || n == 0) break;
        total += n;
    }
    /* echo the TICK line back as a log so the test can see the input arrived */
    char line[80]; u32 k = 0; const char *p = "LOG got ";
    while (*p) line[k++] = *p++;
    for (u32 i = 0; i < total && in[i] != '\n' && k < 70; i++) line[k++] = in[i];
    line[k++] = '\n';
    write_all(1, line, k);
    write_all(1, out, sizeof out - 1);
}
