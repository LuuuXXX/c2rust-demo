CC = gcc
CFLAGS = -Wall -fPIC -shared
TARGET = libhook.so

all: $(TARGET)

$(TARGET): hook.c
	$(CC) $(CFLAGS) -o $(TARGET) hook.c -ldl

clean:
	rm -f $(TARGET)

.PHONY: all clean
