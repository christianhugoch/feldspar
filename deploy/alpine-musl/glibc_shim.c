/* Shims for the glibc-only symbols referenced by rusty_v8's prebuilt
 * *-unknown-linux-gnu archive, so it links against musl (Alpine).
 * Layouts used here (struct stat, dirent, FILE*) are the kernel's on 64-bit
 * Linux and identical between glibc and musl.
 *
 * setup.sh compiles this and appends the object to a copy of the archive, so
 * the linker pulls it in exactly when V8 references one of these symbols.
 * See docs/BUILD_ALPINE_MUSL.md. If a V8 bump adds a new undefined glibc
 * symbol, the link fails naming it: add it here. */
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/sendfile.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>
#include <wchar.h>

/* Large-file aliases: on 64-bit musl off_t is already 64 bits. */
FILE *fopen64(const char *p, const char *m) { return fopen(p, m); }
int fseeko64(FILE *f, off_t o, int w) { return fseeko(f, o, w); }
off_t ftello64(FILE *f) { return ftello(f); }
int ftruncate64(int fd, off_t l) { return ftruncate(fd, l); }
int truncate64(const char *p, off_t l) { return truncate(p, l); }
int getrlimit64(int r, struct rlimit *l) { return getrlimit(r, l); }
int mkstemp64(char *t) { return mkstemp(t); }
void *mmap64(void *a, size_t l, int p, int f, int fd, off_t o) { return mmap(a, l, p, f, fd, o); }
int open64(const char *p, int fl, ...) {
  va_list ap; va_start(ap, fl); mode_t m = va_arg(ap, mode_t); va_end(ap);
  return open(p, fl, m);
}
int openat64(int d, const char *p, int fl, ...) {
  va_list ap; va_start(ap, fl); mode_t m = va_arg(ap, mode_t); va_end(ap);
  return openat(d, p, fl, m);
}
ssize_t pread64(int fd, void *b, size_t n, off_t o) { return pread(fd, b, n, o); }
struct dirent *readdir64(DIR *d) { return readdir(d); }
ssize_t sendfile64(int o, int i, off_t *off, size_t n) { return sendfile(o, i, off, n); }
int statvfs64(const char *p, struct statvfs *b) { return statvfs(p, b); }
FILE *tmpfile64(void) { return tmpfile(); }

/* Pre-2.33 glibc stat entry points. */
int __xstat64(int v, const char *p, struct stat *b) { (void)v; return stat(p, b); }
int __lxstat64(int v, const char *p, struct stat *b) { (void)v; return lstat(p, b); }
int __fxstat64(int v, int fd, struct stat *b) { (void)v; return fstat(fd, b); }

/* _FORTIFY_SOURCE checked variants. */
static void chk_fail(void) { fputs("*** buffer overflow detected ***\n", stderr); abort(); }
void *__memcpy_chk(void *d, const void *s, size_t n, size_t dl) { if (dl < n) chk_fail(); return memcpy(d, s, n); }
void *__memset_chk(void *d, int c, size_t n, size_t dl) { if (dl < n) chk_fail(); return memset(d, c, n); }
size_t __fread_chk(void *p, size_t pl, size_t sz, size_t n, FILE *f) {
  if (sz && n > pl / sz) chk_fail();
  return fread(p, sz, n, f);
}
int __vfprintf_chk(FILE *f, int flag, const char *fmt, va_list ap) { (void)flag; return vfprintf(f, fmt, ap); }
int __vsnprintf_chk(char *s, size_t n, int flag, size_t sl, const char *fmt, va_list ap) {
  (void)flag; if (sl < n) chk_fail(); return vsnprintf(s, n, fmt, ap);
}
size_t __mbrlen(const char *s, size_t n, mbstate_t *ps) { return mbrlen(s, n, ps); }

/* execinfo: musl has none; V8 only uses it for crash dumps. */
int backtrace(void **b, int n) { (void)b; (void)n; return 0; }
char **backtrace_symbols(void *const *b, int n) { (void)b; (void)n; return NULL; }
void backtrace_symbols_fd(void *const *b, int n, int fd) { (void)b; (void)n; (void)fd; }

/* Top of the main thread's stack, approximated from a constructor frame. */
void *__libc_stack_end;
__attribute__((constructor)) static void init_stack_end(void) {
  __libc_stack_end = __builtin_frame_address(0);
}

/* thread_local destructors for libc++abi's __cxa_thread_atexit. */
struct dtor { void (*fn)(void *); void *obj; struct dtor *next; };
static __thread struct dtor *dtors;
static pthread_key_t dtor_key;
static pthread_once_t dtor_once = PTHREAD_ONCE_INIT;
static void run_dtors(void *unused) {
  (void)unused;
  while (dtors) {
    struct dtor *d = dtors; dtors = d->next;
    d->fn(d->obj); free(d);
  }
}
static void make_key(void) { pthread_key_create(&dtor_key, run_dtors); }
int __cxa_thread_atexit_impl(void (*fn)(void *), void *obj, void *dso) {
  (void)dso;
  pthread_once(&dtor_once, make_key);
  struct dtor *d = malloc(sizeof *d);
  if (!d) return -1;
  d->fn = fn; d->obj = obj; d->next = dtors; dtors = d;
  pthread_setspecific(dtor_key, (void *)1);
  return 0;
}
