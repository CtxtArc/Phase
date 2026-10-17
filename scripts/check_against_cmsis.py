#!/usr/bin/env python3
"""Check a PHASE board file against the vendor's CMSIS header.

Usage: check_against_cmsis.py <gen.h> <out.c> <vendor.h> <board.phase>

<gen.h> is the C header `phase emit-c` generated for a program that
imports the board file. This writes <out.c>: a C file that includes the
vendor header (stm32f401xe.h) and the generated one, with a
`_Static_assert` per register (its address in PHASE equals its address in
CMSIS) and per field (the same position and mask). Compiling <out.c>
(scripts/check_against_cmsis.sh does) is the check; the script also lists
any register or field it found no CMSIS counterpart for, which is then
checked by hand. Every `const NAME_IRQn` in <board.phase> is compared with
the vendor header's IRQn enum here, in Python.
"""
import re
import sys

# PHASE device -> (CMSIS instance, CMSIS type). Registers of a DMA stream
# (`CR_3` in PHASE) live in CMSIS's per-stream struct instead.
DEVICES = {
    "Rcc": ("RCC", "RCC_TypeDef"),
    "GpioA": ("GPIOA", "GPIO_TypeDef"),
    "GpioB": ("GPIOB", "GPIO_TypeDef"),
    "GpioC": ("GPIOC", "GPIO_TypeDef"),
    "Usart1": ("USART1", "USART_TypeDef"),
    "Usart2": ("USART2", "USART_TypeDef"),
    "Usart6": ("USART6", "USART_TypeDef"),
    "Dma1": ("DMA1", "DMA_TypeDef"),
    "Dma2": ("DMA2", "DMA_TypeDef"),
    "Adc1": ("ADC1", "ADC_TypeDef"),
    "AdcCommon": ("ADC1_COMMON", "ADC_Common_TypeDef"),
    "Tim2": ("TIM2", "TIM_TypeDef"),
    "Tim3": ("TIM3", "TIM_TypeDef"),
    "Tim4": ("TIM4", "TIM_TypeDef"),
    "Tim5": ("TIM5", "TIM_TypeDef"),
    "Nvic": ("NVIC", "NVIC_Type"),
}
# PHASE device type (or untyped device) -> CMSIS macro prefix.
PREFIX = {
    "Rcc": "RCC",
    "Stm32Gpio": "GPIO",
    "Stm32Usart": "USART",
    "Stm32Dma": "DMA",
    "Adc1": "ADC",
    "AdcCommon": "ADC",
    "Stm32GpTimer": "TIM",
}
# Per-stream DMA registers: PHASE name -> CMSIS DMA_Stream_TypeDef member
# and macro register part.
DMA_STREAM = {"CR": "SxCR", "NDTR": "SxNDT", "PAR": "SxPAR", "M0AR": "SxM0AR",
              "M1AR": "SxM1AR", "FCR": "SxFCR"}


def main():
    gen_h, out_c = sys.argv[1], sys.argv[2]
    text = open(gen_h).read()
    cmsis_text = open(sys.argv[3]).read() if len(sys.argv) > 3 else ""
    cmsis_macros = {m.lower(): m for m in re.findall(r"#define\s+(\w+_Pos)\b", cmsis_text)}

    # Struct members per C type, from the generated typedefs.
    members = {}
    for body, name in re.findall(r"typedef struct \{\n(.*?)\} (\w+);", text, re.S):
        members[name] = re.findall(r"volatile \w+ (\w+);", body)
    # Devices: name, struct type, base address, type name (or own name).
    devices = []
    for name, ty, base in re.findall(r"#define PHASE_DEV_(\w+) \(\((\w+) \*\)(0x[0-9a-f]+)u\)", text):
        m = re.match(r"phase_devtype_(\w+)_regs", ty)
        devices.append((name, ty, int(base, 16), m.group(1) if m else name))

    asserts, unmatched = [], []
    for name, ty, base, tname in devices:
        if name not in DEVICES:
            unmatched.append(f"device {name}")
            continue
        inst, ctype = DEVICES[name]
        for reg in members[ty]:
            arr = re.match(r"(\w+?)_(\d+)$", reg)
            if name.startswith("Dma") and arr and arr.group(1) in DMA_STREAM:
                r, n = arr.groups()
                cms = f"{inst}_Stream{n}_BASE + offsetof(DMA_Stream_TypeDef, {r})"
            elif name == "Nvic" and arr:
                r, n = arr.groups()
                cms = f"{inst}_BASE + offsetof({ctype}, {r}[{n}])"
            elif name.startswith("Gpio") and reg in ("AFRL", "AFRH"):
                cms = f"{inst}_BASE + offsetof({ctype}, AFR[{0 if reg == 'AFRL' else 1}])"
            else:
                cms = f"{inst}_BASE + offsetof({ctype}, {reg})"
            asserts.append(
                f'_Static_assert({base:#x}u + offsetof({ty}, {reg}) == {cms}, "{name}.{reg} is not at CMSIS\'s address");'
            )

    seen = set()
    for full in re.findall(r"#define PHASE_(\w+)_Pos ", text):
        if full in seen:
            continue
        seen.add(full)
        tname = next((t for t in sorted(PREFIX, key=len, reverse=True) if full.startswith(t + "_")), None)
        if tname is None:
            unmatched.append(f"field {full}")
            continue
        rest = full[len(tname) + 1:]
        m = re.match(r"(\w+?)(?:_(\d+))?_(\w+)$", rest)
        reg, idx, field = m.groups()
        # Registers whose PHASE name has an underscore (none today) would
        # need care here; every register name above is one word.
        p = PREFIX[tname]
        regs = [DMA_STREAM.get(reg, reg)] if p == "DMA" else [reg]
        fields = [field, field.replace("MODER", "MODE"), field.replace("ODR", "OD"),
                  field.replace("IDR", "ID"), field.replace("AFSEL", "AFRL")]
        if field == "DIV_FRACTION":
            fields.append("DIV_Fraction")
        if field == "DIV_MANTISSA":
            fields.append("DIV_Mantissa")
        cands = [f"{p}_{r}_{f}_Pos" for r in regs for f in fields]
        if p == "DMA" and reg == "NDTR":
            cands.append("DMA_SxNDT_Pos")
        if p == "ADC" and reg == "DR":
            cands.append("ADC_DR_DATA_Pos")
        hit = next((cmsis_macros[c.lower()] for c in cands if c.lower() in cmsis_macros), None)
        if hit is None:
            unmatched.append(f"field {full}")
            continue
        cm = hit[: -len("_Pos")]
        asserts.append(f'_Static_assert(PHASE_{full}_Pos == {cm}_Pos, "{full}: position differs from {cm}");')
        asserts.append(f'_Static_assert(PHASE_{full}_Msk == {cm}_Msk, "{full}: width differs from {cm}");')

    with open(out_c, "w") as f:
        f.write("/* Generated by scripts/check_against_cmsis.py. */\n")
        f.write("#include <stddef.h>\n#include \"stm32f401xe.h\"\n")
        f.write("#define PHASE_TARGET_HW 1\n#define PHASE_NO_SHORT_NAMES 1\n")
        f.write(f"#include \"{gen_h.rsplit('/', 1)[-1]}\"\n\n")
        f.write("\n".join(asserts) + "\n")
    irq_errors = 0
    if len(sys.argv) > 4:
        board = open(sys.argv[4]).read()
        vendor = dict(re.findall(r"^\s+(\w+_IRQn)\s+=\s+(-?\d+)", cmsis_text, re.M))
        mine = re.findall(r"const (\w+_IRQn): u32 = (\d+);", board)
        for name, n in mine:
            if vendor.get(name) != n:
                print(f"IRQ mismatch: {name} is {n} in the board file, {vendor.get(name)} in CMSIS")
                irq_errors += 1
        print(f"{len(mine)} IRQ numbers checked against CMSIS")
    regs = sum(1 for a in asserts if "address" in a)
    fields = (len(asserts) - regs) // 2
    print(f"{regs} register addresses and {fields} fields to check against CMSIS")
    for u in unmatched:
        print(f"  no CMSIS counterpart found: {u}")
    sys.exit(1 if irq_errors else 0)


if __name__ == "__main__":
    main()
