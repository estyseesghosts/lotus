all: fixture.3dsx

include $(DEVKITPRO)/devkitARM/3ds_rules

LD := $(CC)
ARCH := -march=armv6k -mtune=mpcore -mfloat-abi=hard -mtp=soft
CFLAGS := -g -O1 -Wall -mword-relocations -ffunction-sections $(ARCH) -D__3DS__
CPPFLAGS := -I$(CTRULIB)/include
LDFLAGS := -specs=3dsx.specs -g $(ARCH) -Wl,-Map,fixture.map
LIBPATHS := -L$(CTRULIB)/lib
LIBS := -lctru
OFILES := main.o
DEPSDIR := .

fixture.elf: main.o
main.o: source/main.c source/step.h
	$(CC) $(CPPFLAGS) $(CFLAGS) -c $< -o $@

.PHONY: all fail slow
fail:
	@echo "intentional build failure" >&2
	@exit 1
slow:
	@echo "waiting in build recipe"
	@sleep 30
