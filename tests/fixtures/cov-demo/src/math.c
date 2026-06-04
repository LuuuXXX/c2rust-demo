#include <stdio.h>

int add(int a, int b) {
    return a + b;
}

static int helper(int x) {
    return x * 2;
}

int compute(int n) {
    return helper(n) + add(n, 1);
}
