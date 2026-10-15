/** Module header */
#include <stdio.h>
#include "my_lib.h"

#define MAX_SIZE 256
#define VERSION "1.0"

typedef struct {
    int x;
    int y;
} Point;

typedef enum {
    RED,
    GREEN,
    BLUE,
} Color;

typedef unsigned int uint32;

struct Node {
    int value;
    struct Node *next;
};

/** Add two numbers */
int add(int a, int b);

void process(const char *input, size_t len);

int main(int argc, char **argv) {
    return 0;
}
