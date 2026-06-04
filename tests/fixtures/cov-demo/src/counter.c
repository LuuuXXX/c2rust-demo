#include <stdio.h>

int g_counter = 0;

void increment(void) {
    g_counter++;
}

int get_counter(void) {
    return g_counter;
}
