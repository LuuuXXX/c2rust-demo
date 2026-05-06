/*
 * 相关环境变量定义:
 * 1. C2RUST_PROJECT_ROOT: 工程的根目录，必须存在.
 * 2. C2RUST_FEATURE_ROOT: 构建的每个target都对应一个Feature, 必须存在
 * 3. C2RUST_CC: 编译程序的名字，如果不指定，则为gcc/clang/cc之一.
 * 4. C2RUST_DEBUG: 设置为1时将调试信息输出到stderr.
*/

#define _GNU_SOURCE
#include <errno.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/file.h>
#include <sys/wait.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>

#define MAX_PATH_LEN 8192
#define MAX_CMD_LEN 16384

static const char* C2RUST_PROJECT_ROOT = "C2RUST_PROJECT_ROOT";
static const char* C2RUST_FEATURE_ROOT = "C2RUST_FEATURE_ROOT";
static const char* C2RUST_CC = "C2RUST_CC";
static const char* C2RUST_LD = "C2RUST_LD";
static const char* C2RUST_CC_SKIP = "C2RUST_CC_SKIP";
static const char* C2RUST_LD_SKIP = "C2RUST_LD_SKIP";
static const char* C2RUST_DEBUG_ENV = "C2RUST_DEBUG";

static const char* cc_names[] = {"gcc", "clang", "cc"};
static const char* ld_names[] = {"ld", "lld"};

/* Debug logging – only active when C2RUST_DEBUG is set in the environment. */
#define DBG(...) do { \
        if (getenv(C2RUST_DEBUG_ENV)) { \
                dprintf(2, "[c2rust-hook] " __VA_ARGS__); \
        } \
} while (0)

/* Return the basename part of a path without allocating (pointer into path). */
static const char* path_basename(const char* path) {
        if (!path) return path;
        const char* slash = strrchr(path, '/');
        return slash ? slash + 1 : path;
}

static inline int is_matched(const char* name, const char** names, int len) {
        for (int i = 0; i < len; ++i) {
                if (strcmp(name, names[i]) == 0) {
                        return 1;
                }
        }
        return 0;
}

/*
 * Match the basename of name against the known compiler list or the value of
 * C2RUST_CC.  Using basename allows invocations like /usr/bin/gcc to be
 * recognised without extra configuration.
 */
static inline int is_compiler(const char* name) {
        const char* cc = getenv(C2RUST_CC);
        const char* base = path_basename(name);
        if (!base) return 0;
        if (!cc) {
                return is_matched(base, cc_names, sizeof(cc_names) / sizeof(cc_names[0]));
        } else {
                return strcmp(path_basename(cc), base) == 0;
        }
}

/*
 * Same basename-aware matching for linker names.
 */
static inline int is_linker(const char* name) {
        const char* ld = getenv(C2RUST_LD);
        const char* base = path_basename(name);
        if (!base) return 0;
        if (!ld) {
                return is_matched(base, ld_names, sizeof(ld_names) / sizeof(ld_names[0]));
        } else {
                return strcmp(path_basename(ld), base) == 0;
        }
}

static inline char* path_from(const char* env) {
        const char* path = getenv(env);
        if (!path) {
                return 0;
        }
        return realpath(path, 0);
}

/*
 * Read the current process's command line from /proc/self/cmdline.
 *
 * The file contains NUL-separated arguments.  On success *out_argv is set to a
 * heap-allocated array of strdup'd strings and the function returns the
 * argument count (>= 1).  The caller is responsible for freeing every element
 * and then the array itself.  Returns -1 on any failure.
 */
static int read_proc_cmdline(char*** out_argv) {
        int fd = open("/proc/self/cmdline", O_RDONLY);
        if (fd < 0) {
                DBG("open /proc/self/cmdline failed: errno=%d\n", errno);
                return -1;
        }

        char buf[MAX_CMD_LEN];
        ssize_t n = read(fd, buf, sizeof(buf) - 1);
        close(fd);
        if (n <= 0) {
                DBG("read /proc/self/cmdline returned %zd errno=%d\n", n, errno);
                return -1;
        }
        buf[n] = '\0';

        /* Count NUL-separated tokens. */
        int argc = 0;
        for (ssize_t i = 0; i < n; i++) {
                if (buf[i] == '\0') argc++;
        }
        if (argc == 0) return -1;

        char** argv = malloc((size_t)(argc + 1) * sizeof(char*));
        if (!argv) {
                DBG("malloc failed for argv (%d entries)\n", argc);
                return -1;
        }

        int idx = 0;
        char* p = buf;
        char* end = buf + n;
        while (idx < argc && p < end) {
                char* s = strdup(p);
                if (!s) {
                        DBG("strdup failed at arg %d\n", idx);
                        for (int i = 0; i < idx; i++) free(argv[i]);
                        free(argv);
                        return -1;
                }
                argv[idx++] = s;
                p += strlen(p) + 1;
        }
        argv[idx] = NULL;

        *out_argv = argv;
        return idx;
}

static void free_cmdline(char** argv, int argc) {
        for (int i = 0; i < argc; i++) free(argv[i]);
        free(argv);
}

static inline int is_cfile(const char* file) {
        int len = strlen(file);
        return len > 2 && strcmp(&file[len - 2], ".c") == 0;
}


// 提取-I, -D, -U, -include参数, 和工程目录下的C文件.
// 输入保证extracted, cfiles最少可以保存argc个输入参数.
static int parse_args(int argc, char* argv[], char* extracted[], char* cfiles[]) {
    int cnt = 0;
    for (int i = 1; i < argc; ++i) {
        char* arg = argv[i];
        if (arg[0] != '-') {
                if (is_cfile(arg) && access(arg, R_OK) == 0) {
                    *cfiles = realpath(arg, 0);
                    ++cfiles;
                }
                continue;
        }
        // 这里提取会影响预处理结果的所有参数
        if (arg[1] == 'I' || arg[1] == 'D' || arg[1] == 'U') {
                if (arg[2]) {
                        extracted[cnt++] = arg;
                } else {
                        extracted[cnt++] = arg;
                        ++i;
                        if (i < argc) {
                                extracted[cnt++] = argv[i];
                        }
                }
        } else if (strcmp(&arg[1], "include") == 0 || strcmp(&arg[1], "isystem") == 0 || strcmp(&arg[1], "iquote") == 0) {
                extracted[cnt++] = arg;
                ++i;
                if (i < argc) {
                    extracted[cnt++] = argv[i];
                }
        } else if (strncmp(&arg[1], "std=", 4) == 0 || strcmp(&arg[1], "fshort-enums") == 0) {
                extracted[cnt++] = arg;
        }
    }

    return cnt;
}

static const char* strip_prefix(const char* path, const char* prefix) {
        int prefix_len = strlen(prefix);
        if (strncmp(path, prefix, prefix_len)) {
               return 0;
        } else if (prefix[prefix_len - 1] == '/') {
                return &path[prefix_len];
        } else if (path[prefix_len] == '/') {
                return &path[prefix_len + 1];
        } else {
                return 0;
        }
}

static void save_options(const char* path, int argc, char* argv[]) {
        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0644);
        if (fd == -1) {
                return;
        }
        for (int i = 0; i < argc; ++i) {
            dprintf(fd, "\"%s\" ", argv[i]);
        }
        close(fd);
}

static void preprocess_cfile(const char* cc, int argc, char* argv[], const char* cfile, const char* project_root, const char* feature_root) {
        const char* path = strip_prefix(cfile, project_root); 
        if (!path) {
                DBG("strip_prefix: skipping %s (not under %s)\n", cfile, project_root);
                return;
        }

        // 获取预处理文件名, 后缀从.c修改为.c2rust
        char full_path[MAX_PATH_LEN];
        int full_path_len = snprintf(full_path, sizeof(full_path), "%s/c/%s2rust", feature_root, path);
        if (full_path_len >= (int)sizeof(full_path)) return;

        char* filename = strrchr(full_path, '/');
        if (!filename) return; //绝对路径一定存在.

        // 创建预处理后文件存储路径
        *filename = 0; //忽略文件名
        char cmd[MAX_CMD_LEN];
        int cmd_len = snprintf(cmd, sizeof(cmd), "mkdir -p \"%s\"", full_path);
        if (cmd_len >= MAX_CMD_LEN) return;
        system(cmd);
        *filename = '/'; //恢复文件名.

        // 需要存储编译选项，bindgen的时候会用上, 存储的文件名后缀为.c2rust.opts
        int len = snprintf(&full_path[full_path_len], sizeof(full_path) - full_path_len, ".opts");
        if (len < (int)(sizeof(full_path) - full_path_len)) {
                // 如果没有生成这个文件，也继续.
                save_options(full_path, argc, argv);
        }
        full_path[full_path_len] = 0;

        DBG("preprocessing %s -> %s\n", cfile, full_path);

        // 预处理命令, gcc和clang有差异. 不能强制用clang来替代，如果当前是gcc会导致混合构建的时候出错.
        // clang解析gcc生成的文件可能出现错误，但是仍然能够生成json文件, 具有一定容错性.
        // -P避免生成行号信息,混合构建时定位信息指向新生成的文件.

        pid_t pid = fork();
        if (pid == 0) {
            const char* new_argv[argc + 8];
            int pos = 0;
            new_argv[pos++] = cc;
            new_argv[pos++] = "-E";
            new_argv[pos++] = "-C";
            new_argv[pos++] = cfile;
            new_argv[pos++] = "-o";
            new_argv[pos++] = full_path;
            new_argv[pos++] = "-P";
            for (int i = 0; i < argc; ++i) {
                    new_argv[pos++] = argv[i];
            }
            new_argv[pos++] = 0;
            execvp(cc, (char**)new_argv);
            _exit(127);
        } else if (pid != -1) {
                int wstatus = 0;
                waitpid(pid, &wstatus, 0);
                if (!WIFEXITED(wstatus) || WEXITSTATUS(wstatus) != 0) {
                        DBG("preprocessing failed for %s: exit=%d\n",
                            cfile, WIFEXITED(wstatus) ? WEXITSTATUS(wstatus) : -1);
                }
        } else {
                DBG("fork failed: errno=%d\n", errno);
        }
}

static void discover_cfile(int argc, char* argv[], const char* project_root, const char* feature_root) {
        if (argc <= 0) return;

        char** cflags = calloc((size_t)argc, sizeof(char*)); // 保存-I, -D, -U, -include
        char** cfiles = calloc((size_t)argc, sizeof(char*)); // 保存当前编译的C文件.
        if (!cflags || !cfiles) {
                DBG("calloc failed for cflags/cfiles\n");
                goto fail;
        }

        if (getenv(C2RUST_CC_SKIP)) goto fail;

        int cnt = parse_args(argc, argv, cflags, cfiles);
        if (!cfiles[0]) {
                DBG("no .c files found in argv[0..%d]\n", argc - 1);
                goto fail;
        }

        setenv(C2RUST_CC_SKIP, "1", 0);

        for (int i = 0; i < argc; ++i) {
                const char* file = cfiles[i];
                if (!file) break;
                preprocess_cfile(argv[0], cnt, cflags, file, project_root, feature_root);
        }
fail:
        if (cfiles) {
                for (char** cf = cfiles; *cf; ++cf) free(*cf);
                free(cfiles);
        }
        if (cflags) free(cflags);
}

// 提取生成的全部动态库和可执行程序的名字，以及生成过程中链接的C2RUST_PROJECT_ROOT目录下的静态库.
char* get_file(char* path) {
        char* deli = strrchr(path, '/');
        return deli ? deli + 1 : path;
}

char* get_static_lib(char* path, const char* project_root) {
        // 判断文件是否存在，如果存在是否在C2RUST_PROJECT_ROOT目录下.
        char* real_path = realpath(path, 0);
        if (!real_path) return 0;
        const char* tmp = strip_prefix(real_path, project_root);
        free(real_path);
        if (!tmp) return 0;
        
        // 提取文件名，判断是否是lib<...>.a
        char* lib = get_file(path);
        if (strncmp(lib, "lib", 3) != 0) return 0;
        int len = strlen(lib);
        if (len > 5 && strcmp(&lib[len - 2], ".a") != 0) return 0;
        return lib;
}

static void target_save(char* libs[], int cnt, const char* feature_root) {
        if (cnt == 0) return;

        setenv(C2RUST_LD_SKIP, "1", 0);

        char buf[MAX_CMD_LEN];
        int len = snprintf(buf, MAX_CMD_LEN, "mkdir -p %s/c", feature_root);
        if (len >= MAX_CMD_LEN) {
                dprintf(2, "command is too long: %s...\n", buf);
                return;
        }
        system(buf);

        len = snprintf(buf, MAX_CMD_LEN, "%s/c/targets.list", feature_root);
        if (len >= MAX_CMD_LEN) {
                dprintf(2, "path is too long: %s...\n", buf);
                return;
        }

        int fd = open(buf, O_CREAT | O_RDWR, 0666);
        if (fd == -1) {
                dprintf(2, "failed to open file: %s...\n", buf);
                return;
        }

        if (flock(fd, LOCK_EX) != 0) {
                dprintf(2, "failed to lock file: %s, errno = %d\n", buf, errno);
                goto fail;
        }

        char* content = &buf[len + 1];
        ssize_t content_len = read(fd, content, MAX_CMD_LEN - len - 1);
        if (content_len == -1) {
                dprintf(2, "failed to read file: %s, errno = %d\n", buf, errno);
                goto fail;
        }
        if (content_len > 0) {
            content[content_len - 1] = 0; //最后总是写入换行符.
        }

        lseek(fd, 0, SEEK_END);

        for (int i = 0; i < cnt; ++i) {
            if (content_len > 0 && !strstr(content, libs[i])) {
                dprintf(fd, "%s\n", libs[i]);
            }
        }
fail:
        close(fd);
}

static void discover_target(int argc, char* argv[], const char* project_root, const char* feature_root) {
        if (argc <= 0) return;

        char** libs = calloc((size_t)argc, sizeof(char*));
        if (!libs) return;
        int pos = 0;

        if (getenv(C2RUST_LD_SKIP)) goto fail;

        for (int i = 1; i < argc; ++i) {
                char* static_lib = get_static_lib(argv[i], project_root);
                if (static_lib) {
                        libs[pos++] = static_lib;
                } else if (strncmp(argv[i], "-o", 2) == 0) {
                        if (argv[i][2] == 0 && i < argc - 1) {
                            libs[pos++] = get_file(argv[i + 1]);
                        } else if (argv[i][2]) {
                            libs[pos++] = get_file(&argv[i][2]);
                        }
                }
        }
        target_save(libs, pos, feature_root);
fail:
        free(libs);
}

/*
 * Library constructor: called automatically when this shared library is loaded
 * into a new process via LD_PRELOAD.
 *
 * Instead of relying on the (non-standard) argc/argv parameters that some
 * constructor implementations pass, we read the real command line from
 * /proc/self/cmdline so that the hook works reliably across different glibc
 * versions, hardened environments, and absolute-path compiler invocations
 * (e.g. /usr/bin/gcc).
 */
__attribute__((constructor)) static void c2rust_hook(void) {
        char* project_root = 0;
        char* feature_root = 0;
        char** argv = NULL;
        int argc = -1;

        project_root = path_from(C2RUST_PROJECT_ROOT);
        if (!project_root) {
                return;
        }

        feature_root = path_from(C2RUST_FEATURE_ROOT);
        if (!feature_root) {
                goto fail;
        }

        argc = read_proc_cmdline(&argv);
        if (argc < 1 || !argv || !argv[0]) {
                DBG("failed to read /proc/self/cmdline\n");
                goto fail;
        }

        DBG("invoked as %s (argc=%d)\n", argv[0], argc);

        if (is_compiler(argv[0])) {
               discover_cfile(argc, argv, project_root, feature_root);
        } else if (is_linker(argv[0])) {
               discover_target(argc, argv, project_root, feature_root);
        } else {
               DBG("not a compiler or linker: %s\n", argv[0]);
        }
fail:
        if (argv) free_cmdline(argv, argc > 0 ? argc : 0);
        if (project_root) free(project_root);
        if (feature_root) free(feature_root);
}
