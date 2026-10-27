// SPDX-FileCopyrightText: 2026 HiFiPhile
// SPDX-License-Identifier: GPL-3.0-only

// Transforms a camera YUV_420_888 frame into planar 4:2:0 and compresses it
// with libjpeg-turbo. The transform is given as row walks computed by
// YuvTransform: for each output row, the index of its first sample and the
// index step per pixel. Each thread keeps its own compressor and buffers.

#include <jni.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <turbojpeg.h>

struct state {
    tjhandle tj;
    int width;
    int height;
    int quality;
    uint8_t *planes[3];
    uint8_t *jpeg;
    size_t jpeg_capacity;
};

static pthread_key_t state_key;
static pthread_once_t state_once = PTHREAD_ONCE_INIT;

static void state_free(void *p) {
    struct state *s = p;
    if (!s) {
        return;
    }
    if (s->tj) {
        tj3Destroy(s->tj);
    }
    free(s->planes[0]);
    tj3Free(s->jpeg);
    free(s);
}

static void state_key_init(void) {
    pthread_key_create(&state_key, state_free);
}

static struct state *state_get(int width, int height, int quality) {
    pthread_once(&state_once, state_key_init);
    struct state *s = pthread_getspecific(state_key);
    if (s && s->width == width && s->height == height && s->quality == quality) {
        return s;
    }
    state_free(s);
    pthread_setspecific(state_key, NULL);

    s = calloc(1, sizeof(*s));
    if (!s) {
        return NULL;
    }
    s->width = width;
    s->height = height;
    s->quality = quality;
    s->tj = tj3Init(TJINIT_COMPRESS);
    s->planes[0] = malloc((size_t)width * height * 3 / 2);
    s->jpeg_capacity = tj3JPEGBufSize(width, height, TJSAMP_420);
    s->jpeg = s->jpeg_capacity ? tj3Alloc(s->jpeg_capacity) : NULL;
    if (!s->tj || !s->planes[0] || !s->jpeg ||
        tj3Set(s->tj, TJPARAM_QUALITY, quality) ||
        tj3Set(s->tj, TJPARAM_SUBSAMP, TJSAMP_420) ||
        tj3Set(s->tj, TJPARAM_NOREALLOC, 1)) {
        state_free(s);
        return NULL;
    }
    s->planes[1] = s->planes[0] + (size_t)width * height;
    s->planes[2] = s->planes[1] + (size_t)width * height / 4;
    pthread_setspecific(state_key, s);
    return s;
}

/** Whether every sample [rows] walks of [count] pixels read lies in [0, len). */
static int walks_in_bounds(const jint *starts, const jint *steps, int rows, int count, jsize len) {
    for (int r = 0; r < rows; r++) {
        int64_t first = starts[r];
        int64_t last = first + (int64_t)steps[r] * (count - 1);
        if (first < 0 || last < 0 || first >= len || last >= len) {
            return 0;
        }
    }
    return 1;
}

// Rows walked together when they step across source rows (90° and 270°),
// so each source cache line serves the whole block instead of one sample.
#define BLOCK 16

/** Whether rows [r, r + n) have the same step. */
static int is_block(const jint *steps, int r, int n) {
    for (int k = 1; k < n; k++) {
        if (steps[r + k] != steps[r]) {
            return 0;
        }
    }
    return 1;
}

static void walk_luma(uint8_t *out, const uint8_t *src, const jint *starts, const jint *steps,
                      int width, int height) {
    for (int r = 0; r < height;) {
        int step = steps[r];
        if (step != 1 && step != -1 && r + BLOCK <= height && is_block(steps, r, BLOCK)) {
            for (int c = 0; c < width; c++) {
                for (int k = 0; k < BLOCK; k++) {
                    out[(size_t)(r + k) * width + c] = src[starts[r + k] + (ptrdiff_t)c * step];
                }
            }
            r += BLOCK;
            continue;
        }

        uint8_t *o = out + (size_t)r * width;
        const uint8_t *p = src + starts[r];
        if (step == 1) {
            memcpy(o, p, width);
        } else if (step == -1) {
            for (int c = 0; c < width; c++) {
                o[c] = p[-c];
            }
        } else {
            for (int c = 0; c < width; c++, p += step) {
                o[c] = *p;
            }
        }
        r++;
    }
}

static void walk_chroma(uint8_t *out_u, uint8_t *out_v, const uint8_t *u, const uint8_t *v,
                        const jint *starts, const jint *steps, int width, int height) {
    for (int r = 0; r < height;) {
        int step = steps[r];
        int n = r + BLOCK <= height && is_block(steps, r, BLOCK) ? BLOCK : 1;
        for (int c = 0; c < width; c++) {
            for (int k = 0; k < n; k++) {
                ptrdiff_t i = starts[r + k] + (ptrdiff_t)c * step;
                out_u[(size_t)(r + k) * width + c] = u[i];
                out_v[(size_t)(r + k) * width + c] = v[i];
            }
        }
        r += n;
    }
}

static void throw_ise(JNIEnv *env, const char *message) {
    jclass cls = (*env)->FindClass(env, "java/lang/IllegalStateException");
    if (cls) {
        (*env)->ThrowNew(env, cls, message);
    }
}

JNIEXPORT jbyteArray JNICALL
Java_com_hifiphile_uvcamera_camera_JpegEncoder_nativeEncode(
    JNIEnv *env, jclass clazz, jint width, jint height, jint quality,
    jbyteArray y_array, jbyteArray u_array, jbyteArray v_array,
    jintArray y_starts_array, jintArray y_steps_array,
    jintArray c_starts_array, jintArray c_steps_array) {
    (void)clazz;

    struct state *s = state_get(width, height, quality);
    if (!s) {
        throw_ise(env, "Failed to set up the JPEG encoder");
        return NULL;
    }

    jsize y_len = (*env)->GetArrayLength(env, y_array);
    jsize u_len = (*env)->GetArrayLength(env, u_array);
    jsize v_len = (*env)->GetArrayLength(env, v_array);
    if ((*env)->GetArrayLength(env, y_starts_array) < height ||
        (*env)->GetArrayLength(env, y_steps_array) < height ||
        (*env)->GetArrayLength(env, c_starts_array) < height / 2 ||
        (*env)->GetArrayLength(env, c_steps_array) < height / 2) {
        throw_ise(env, "Row walks too short");
        return NULL;
    }

    // No other JNI calls until these are released.
    uint8_t *y = (*env)->GetPrimitiveArrayCritical(env, y_array, NULL);
    uint8_t *u = (*env)->GetPrimitiveArrayCritical(env, u_array, NULL);
    uint8_t *v = (*env)->GetPrimitiveArrayCritical(env, v_array, NULL);
    jint *y_starts = (*env)->GetPrimitiveArrayCritical(env, y_starts_array, NULL);
    jint *y_steps = (*env)->GetPrimitiveArrayCritical(env, y_steps_array, NULL);
    jint *c_starts = (*env)->GetPrimitiveArrayCritical(env, c_starts_array, NULL);
    jint *c_steps = (*env)->GetPrimitiveArrayCritical(env, c_steps_array, NULL);

    int ok = y && u && v && y_starts && y_steps && c_starts && c_steps &&
        walks_in_bounds(y_starts, y_steps, height, width, y_len) &&
        walks_in_bounds(c_starts, c_steps, height / 2, width / 2, u_len < v_len ? u_len : v_len);
    if (ok) {
        walk_luma(s->planes[0], y, y_starts, y_steps, width, height);
        walk_chroma(s->planes[1], s->planes[2], u, v, c_starts, c_steps, width / 2, height / 2);
    }

    if (c_steps) (*env)->ReleasePrimitiveArrayCritical(env, c_steps_array, c_steps, JNI_ABORT);
    if (c_starts) (*env)->ReleasePrimitiveArrayCritical(env, c_starts_array, c_starts, JNI_ABORT);
    if (y_steps) (*env)->ReleasePrimitiveArrayCritical(env, y_steps_array, y_steps, JNI_ABORT);
    if (y_starts) (*env)->ReleasePrimitiveArrayCritical(env, y_starts_array, y_starts, JNI_ABORT);
    if (v) (*env)->ReleasePrimitiveArrayCritical(env, v_array, v, JNI_ABORT);
    if (u) (*env)->ReleasePrimitiveArrayCritical(env, u_array, u, JNI_ABORT);
    if (y) (*env)->ReleasePrimitiveArrayCritical(env, y_array, y, JNI_ABORT);
    if (!ok) {
        throw_ise(env, "Row walks out of bounds");
        return NULL;
    }

    const uint8_t *planes[3] = { s->planes[0], s->planes[1], s->planes[2] };
    int strides[3] = { width, width / 2, width / 2 };
    size_t size = s->jpeg_capacity;
    if (tj3CompressFromYUVPlanes8(s->tj, planes, width, strides, height, &s->jpeg, &size)) {
        throw_ise(env, tj3GetErrorStr(s->tj));
        return NULL;
    }

    jbyteArray result = (*env)->NewByteArray(env, (jsize)size);
    if (result) {
        (*env)->SetByteArrayRegion(env, result, 0, (jsize)size, (const jbyte *)s->jpeg);
    }
    return result;
}
