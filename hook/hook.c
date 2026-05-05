/*
 * 相关环境变量定义:
 * 1. C2RUST_PROJECT_ROOT: 工程的根目录，必须存在.
 * 2. C2RUST_FEATURE_ROOT: 构建的每个target都对应一个Feature, 必须存在
 * 3. C2RUST_CC: 编译程序的名字，如果不指定，则为gcc/clang/cc之一.
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

static const char* cc_names[] = {"gcc", "clang", "cc"};
static const char* ld_names[] = {"ld", "lld"};

static inline int is_matched(const char* name, const char** names, int len) {
        for (int i = 0; i < len; ++i) {
                if (strcmp(name, names[i]) == 0) {
                        return 1;
                }
        }
        return 0;
}

static inline int is_compiler(const char* name) {
        const char* cc = getenv(C2RUST_CC);
        if (!cc) {
            return is_matched(name, cc_names, sizeof(cc_names) / sizeof(cc_names[0]));
        } else {
            return strcmp(cc, name) == 0;
        }
}

static inline int is_linker(const char* name) {
        const char* ld = getenv(C2RUST_LD);
        if (!ld) {
            return is_matched(name, ld_names, sizeof(ld_names) / sizeof(ld_names[0]));
        } else {
            return strcmp(ld, name) == 0;
        }
}

static inline char* path_from(const char* env) {
        const char* path = getenv(env);
        if (!path) {
                return 0;
        }
        return realpath(path, 0);
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
        if (!path) return;

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
                waitpid(pid, 0, 0);
        }
}

static void discover_cfile(int argc, char* argv[], const char* project_root, const char* feature_root) {
        char* cflags[argc]; // 保存-I, -D, -U, -include
        char* cfiles[argc]; // 保存当前编译的C文件.


        if (getenv(C2RUST_CC_SKIP)) return;

        memset(cflags, 0, sizeof(char*) * argc);
        memset(cfiles, 0, sizeof(char*) * argc);

        int cnt = parse_args(argc, argv, cflags, cfiles);
        if (!cfiles[0]) {
                goto fail;
        }

        setenv(C2RUST_CC_SKIP, "1", 0);

        for (int i = 0; i < argc; ++i) {
                const char* file = cfiles[i];
                if (!file) break;
                preprocess_cfile(argv[0], cnt, cflags, file, project_root, feature_root);
        }
fail:
        for (char** cfile = cfiles; *cfile; ++cfile) {
                free(*cfile);
        }
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
            if (content > 0 && !strstr(content, libs[i])) {
                dprintf(fd, "%s\n", libs[i]);
            }
        }
fail:
        close(fd);
}

static void discover_target(int argc, char* argv[], const char* project_root, const char* feature_root) {
        char* libs[argc];
        int pos = 0;

        if (getenv(C2RUST_LD_SKIP)) return;

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
}

__attribute__((constructor)) static void c2rust_hook(int argc, char* argv[]) {
        char* project_root = 0;
        char* feature_root = 0;
        project_root = path_from(C2RUST_PROJECT_ROOT);
        if (!project_root) {
                return;
        }

        feature_root = path_from(C2RUST_FEATURE_ROOT);
        if (!feature_root) {
                goto fail;
        }
        
        if (is_compiler(program_invocation_short_name)) {
               discover_cfile(argc, argv, project_root, feature_root);
        } else if (is_linker(program_invocation_short_name)) {
               discover_target(argc, argv, project_root, feature_root);
        }
fail:
        if (project_root) free(project_root);
        if (feature_root) free(feature_root);
}
