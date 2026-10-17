# Build a vendor-style STM32F401 project whose TIM2 driver is written in
# PHASE. Needs arm-none-eabi-gcc, the `phase` compiler, and ST's CMSIS
# headers and startup files:
#
#   git clone https://github.com/STMicroelectronics/cmsis-device-f4
#   git clone --filter=blob:none --sparse https://github.com/ARM-software/CMSIS_5
#   make CMSIS_DEVICE=cmsis-device-f4 CMSIS_CORE=CMSIS_5/CMSIS/Core
#
# (scripts/check_against_cmsis.sh downloads the same two, pinned, into
# ~/.cache/phase-cmsis.)

PHASE        ?= phase
CMSIS_DEVICE ?= $(HOME)/.cache/phase-cmsis/cmsis-device-f4
CMSIS_CORE   ?= $(HOME)/.cache/phase-cmsis/CMSIS_5/CMSIS/Core
CC           := arm-none-eabi-gcc
OBJCOPY      := arm-none-eabi-objcopy
BUILD        := build

CFLAGS := -mcpu=cortex-m4 -mthumb -mfloat-abi=soft -O1 -std=c11 -Wall -Wextra \
          -DSTM32F401xE -DPHASE_TARGET_HW -DPHASE_NO_SHORT_NAMES \
          -I$(BUILD) -I$(CMSIS_DEVICE)/Include -I$(CMSIS_CORE)/Include
LDFLAGS := -T stm32f401re.ld -nostartfiles --specs=nano.specs -Wl,--gc-sections -Wl,--no-warn-rwx-segments

all: $(BUILD)/app.bin

# The PHASE driver: checked by the compiler, then plain C like the rest.
$(BUILD)/ticker.gen.c $(BUILD)/ticker.gen.h: ticker.phase ../../devices/nucleo_f401re.phase
	$(PHASE) emit-c --out-dir=$(BUILD) ticker.phase

$(BUILD)/app.elf: main.c $(BUILD)/ticker.gen.c $(BUILD)/ticker.gen.h stm32f401re.ld
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ main.c $(BUILD)/ticker.gen.c \
	    $(CMSIS_DEVICE)/Source/Templates/system_stm32f4xx.c \
	    $(CMSIS_DEVICE)/Source/Templates/gcc/startup_stm32f401xe.s

$(BUILD)/app.bin: $(BUILD)/app.elf
	$(OBJCOPY) -O binary $< $@

clean:
	rm -rf $(BUILD)

.PHONY: all clean
