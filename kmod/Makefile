# SPDX-License-Identifier: GPL-2.0

# Out-of-tree build against a prepared kernel tree:
#   make -C kmod KERNEL_SRC=/path/to/kernel LLVM=1 ARCH=arm64
KERNEL_SRC ?= /lib/modules/$(shell uname -r)/build

all:
	$(MAKE) -C $(KERNEL_SRC) M=$(CURDIR) modules

clean:
	$(MAKE) -C $(KERNEL_SRC) M=$(CURDIR) clean

.PHONY: all clean
