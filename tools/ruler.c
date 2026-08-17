/*
 * Ruler test for the doubled MTK framebuffer.
 *
 * Draws distinct patterns in each half of the fb and refreshes both:
 *   TOP half    : white bg, black border + corner crosses + center cross
 *   BOTTOM half : black bg, white border + corner crosses + center cross
 * Whichever pattern you see tells us which half the panel actually displays.
 *
 * Build: zig cc -target arm-linux-musleabihf -static -O2 -o ruler ruler.c
 * Run on-device (as root): /mnt/us/yb/ruler
 */
#include <fcntl.h>
#include <linux/ioctl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define FB "/dev/fb0"
#define W 1236
#define H 1648
#define STRIDE 1248

static void cross(unsigned char *buf, int x, int y, int len, unsigned char color) {
    for (int i = -len; i <= len; i++) {
        int xx = x + i, yy = y + i;
        if (xx >= 0 && xx < W)
            buf[y * STRIDE + xx] = color;
        if (yy >= 0 && yy < H)
            buf[yy * STRIDE + x] = color;
    }
}

static void border(unsigned char *buf, int t, int l, int b, int r, int thick,
                   unsigned char color) {
    for (int i = 0; i < thick; i++) {
        for (int x = l; x <= r; x++) {
            buf[t * STRIDE + x] = color;
            buf[b * STRIDE + x] = color;
        }
        for (int y = t; y <= b; y++) {
            buf[y * STRIDE + l] = color;
            buf[y * STRIDE + r] = color;
        }
        t++;
        l++;
        b--;
        r--;
    }
}

/* Mirror of ybdev's MxcfbUpdateDataMtk (96 bytes). */
struct mtk_update {
    unsigned int update_region_top, update_region_left, update_region_width,
        update_region_height;
    unsigned int waveform_mode, update_mode, update_marker;
    int temp;
    unsigned int flags, dither_mode, quant_bit;
    unsigned int alt_phys, alt_width, alt_height;
    unsigned int alt_region_top, alt_region_left, alt_region_width,
        alt_region_height;
    unsigned int swipe_direction, swipe_steps;
    unsigned int hist_bw, hist_gray, ts_pxp, ts_epdc;
};

int main(void) {
    int fd = open(FB, O_RDWR);
    if (fd < 0) {
        perror("open");
        return 1;
    }
    size_t map_len = (size_t)STRIDE * H * 2;
    unsigned char *fb = mmap(NULL, map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (fb == MAP_FAILED) {
        perror("mmap");
        return 1;
    }

    unsigned char *top = fb;
    unsigned char *bot = fb + (size_t)STRIDE * H;

    memset(top, 255, (size_t)STRIDE * H); /* white */
    memset(bot, 0, (size_t)STRIDE * H);   /* black */

    border(top, 4, 4, H - 5, W - 5, 3, 0);
    cross(top, 0, 0, 20, 0);
    cross(top, W - 1, 0, 20, 0);
    cross(top, 0, H - 1, 20, 0);
    cross(top, W - 1, H - 1, 20, 0);
    cross(top, W / 2, H / 2, 30, 0);

    border(bot, 4, 4, H - 5, W - 5, 3, 255);
    cross(bot, 0, 0, 20, 255);
    cross(bot, W - 1, 0, 20, 255);
    cross(bot, 0, H - 1, 20, 255);
    cross(bot, W - 1, H - 1, 20, 255);
    cross(bot, W / 2, H / 2, 30, 255);

    struct mtk_update u;
    memset(&u, 0, sizeof(u));
    u.waveform_mode = 2; /* GC16 */
    u.update_mode = 1;   /* FULL */
    u.temp = 0x1000;
    u.hist_bw = 1; /* DU */
    u.hist_gray = 2;
    u.update_region_width = W;
    u.update_region_height = H;
    unsigned long req = _IOW('F', 0x2E, struct mtk_update);

    u.update_marker = 1;
    u.update_region_top = 0;
    ioctl(fd, req, &u);

    u.update_marker = 2;
    u.update_region_top = H;
    ioctl(fd, req, &u);

    munmap(fb, map_len);
    close(fd);
    return 0;
}

