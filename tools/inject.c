/*
 * Synthetic touch injector for the Kindle (dev loop tool).
 *
 * Root can write evdev events into /dev/input/event1 (the pt_mt panel) —
 * the same trick KOReader's fakeTapInput uses — so we can drive the UI
 * deterministically over SSH without touching the screen.
 *
 * Build (armhf, static):
 *   zig cc -target arm-linux-musleabihf -static -O2 -o inject inject.c
 *
 * Usage:
 *   inject x y                 tap at (x, y)
 *   inject x1 y1 x2 y2 steps   swipe from (x1,y1) to (x2,y2)
 */
#include <fcntl.h>
#include <linux/input.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/time.h>
#include <unistd.h>

static void emit(int fd, unsigned short type, unsigned short code, int val) {
    struct input_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.type = type;
    ev.code = code;
    ev.value = val;
    if (write(fd, &ev, sizeof(ev)) != (ssize_t)sizeof(ev)) {
        perror("write");
    }
}

static void syn(int fd) {
    emit(fd, EV_SYN, SYN_REPORT, 0);
}

int main(int argc, char **argv) {
    const char *dev = "/dev/input/event1";
    int x1 = argc > 1 ? atoi(argv[1]) : 618;
    int y1 = argc > 2 ? atoi(argv[2]) : 600;
    int x2 = argc > 3 ? atoi(argv[3]) : x1;
    int y2 = argc > 4 ? atoi(argv[4]) : y1;
    int steps = argc > 5 ? atoi(argv[5]) : 1;

    int fd = open(dev, O_WRONLY);
    if (fd < 0) {
        perror("open");
        return 1;
    }

    emit(fd, EV_ABS, ABS_MT_SLOT, 0);
    emit(fd, EV_ABS, ABS_MT_TRACKING_ID, 1);
    for (int i = 0; i <= steps; i++) {
        int cx = x1 + (x2 - x1) * i / steps;
        int cy = y1 + (y2 - y1) * i / steps;
        emit(fd, EV_ABS, ABS_MT_POSITION_X, cx);
        emit(fd, EV_ABS, ABS_MT_POSITION_Y, cy);
        syn(fd);
        usleep(20000);
    }
    emit(fd, EV_ABS, ABS_MT_TRACKING_ID, -1);
    syn(fd);

    close(fd);
    return 0;
}
