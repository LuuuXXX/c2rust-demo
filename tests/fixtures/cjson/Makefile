CC = gcc
CFLAGS = -Wall

all: libcjson.a

libcjson.a: cJSON.o
	ar rcs $@ $^

cJSON.o: cJSON.c cJSON.h
	$(CC) $(CFLAGS) -c -o $@ $<

clean:
	rm -f cJSON.o libcjson.a

.PHONY: all clean
