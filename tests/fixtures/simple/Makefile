CC = gcc
CFLAGS = -Wall

SRCS = src/math.c src/counter.c
OBJS = $(SRCS:.c=.o)

all: libsimple.a

libsimple.a: $(OBJS)
	ar rcs $@ $^

%.o: %.c
	$(CC) $(CFLAGS) -c -o $@ $<

clean:
	rm -f $(OBJS) libsimple.a

.PHONY: all clean
