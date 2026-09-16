/* RP2350A, as fitted to a Pico 2 H: 4 MB of QSPI flash and 520 KB of SRAM,
   of which the 512 KB striped bank at 0x20000000 is the contiguous part.

   The three INSERTed sections are not decoration. The RP2350 boot ROM looks
   for an IMAGE_DEF block near the start of flash and refuses to run an image
   without one — a chip that sits in BOOTSEL forever rather than saying why,
   which is the failure this file exists to prevent. `src/main.rs` supplies
   the block; this puts it where the ROM looks. */

MEMORY {
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K
    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS {
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
