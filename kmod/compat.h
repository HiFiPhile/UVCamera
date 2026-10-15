/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Definitions the upstream UVC gadget needs that older kernels lack.
 * Force-included into every file of the module.
 */
#ifndef UVCX_COMPAT_H
#define UVCX_COMPAT_H

#include <linux/version.h>
#include <linux/slab.h>
#include <linux/fs.h>
#include <linux/usb/video.h>
#include <media/v4l2-fh.h>

#if LINUX_VERSION_CODE < KERNEL_VERSION(7, 0, 0)
#define kzalloc_obj(obj) kzalloc(sizeof(obj), GFP_KERNEL)
#endif

#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 14, 0)
#include <linux/kthread.h>
/* Before 6.14 the create function also started the worker. */
#define kthread_run_worker kthread_create_worker
#endif

#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 18, 0)
static inline struct v4l2_fh *uvcx_file_to_v4l2_fh(struct file *filp)
{
	return filp->private_data;
}
#define file_to_v4l2_fh uvcx_file_to_v4l2_fh

static inline void uvcx_v4l2_fh_add(struct v4l2_fh *fh, struct file *filp)
{
	v4l2_fh_add(fh);
	filp->private_data = fh;
}

static inline void uvcx_v4l2_fh_del(struct v4l2_fh *fh, struct file *filp)
{
	v4l2_fh_del(fh);
	filp->private_data = NULL;
}
#define v4l2_fh_add uvcx_v4l2_fh_add
#define v4l2_fh_del uvcx_v4l2_fh_del
#endif

/*
 * Android GKI kernels only export the symbols their vendor modules use, which
 * leaves this V4L2 helper out. A copy from v4l2-common.c.
 */
#include <media/v4l2-common.h>
static inline u32 uvcx_fraction_to_interval(u32 numerator, u32 denominator)
{
	u32 multiplier;

	/* Saturate the result if the operation would overflow. */
	if (denominator == 0 ||
	    numerator/denominator >= ((u32)-1)/10000000)
		return (u32)-1;

	/*
	 * Divide both the denominator and the multiplier by two until
	 * numerator * multiplier doesn't overflow. If anyone knows a better
	 * algorithm please let me know.
	 */
	multiplier = 10000000;
	while (numerator > ((u32)-1)/multiplier) {
		multiplier /= 2;
		denominator /= 2;
	}

	return denominator ? numerator * multiplier / denominator : 0;
}
#define v4l2_fraction_to_interval uvcx_fraction_to_interval

#if LINUX_VERSION_CODE < KERNEL_VERSION(6, 13, 0)
/* Frame Based Payload - 3.1.1. Frame Based Video Format Descriptor */
struct uvc_format_framebased {
	__u8  bLength;
	__u8  bDescriptorType;
	__u8  bDescriptorSubType;
	__u8  bFormatIndex;
	__u8  bNumFrameDescriptors;
	__u8  guidFormat[16];
	__u8  bBitsPerPixel;
	__u8  bDefaultFrameIndex;
	__u8  bAspectRatioX;
	__u8  bAspectRatioY;
	__u8  bmInterfaceFlags;
	__u8  bCopyProtect;
	__u8  bVariableSize;
} __attribute__((__packed__));

#define UVC_DT_FORMAT_FRAMEBASED_SIZE			28

/* Frame Based Payload - 3.1.2. Frame Based Video Frame Descriptor */
struct uvc_frame_framebased {
	__u8  bLength;
	__u8  bDescriptorType;
	__u8  bDescriptorSubType;
	__u8  bFrameIndex;
	__u8  bmCapabilities;
	__u16 wWidth;
	__u16 wHeight;
	__u32 dwMinBitRate;
	__u32 dwMaxBitRate;
	__u32 dwDefaultFrameInterval;
	__u8  bFrameIntervalType;
	__u32 dwBytesPerLine;
	__u32 dwFrameInterval[];
} __attribute__((__packed__));

#define UVC_DT_FRAME_FRAMEBASED_SIZE(n)			(26+4*(n))

#define UVC_FRAME_FRAMEBASED(n) \
	uvc_frame_framebased_##n

#define DECLARE_UVC_FRAME_FRAMEBASED(n)			\
struct UVC_FRAME_FRAMEBASED(n) {			\
	__u8  bLength;					\
	__u8  bDescriptorType;				\
	__u8  bDescriptorSubType;			\
	__u8  bFrameIndex;				\
	__u8  bmCapabilities;				\
	__u16 wWidth;					\
	__u16 wHeight;					\
	__u32 dwMinBitRate;				\
	__u32 dwMaxBitRate;				\
	__u32 dwDefaultFrameInterval;			\
	__u8  bFrameIntervalType;			\
	__u32 dwBytesPerLine;				\
	__u32 dwFrameInterval[n];			\
} __attribute__ ((packed))
#endif

#endif /* UVCX_COMPAT_H */
