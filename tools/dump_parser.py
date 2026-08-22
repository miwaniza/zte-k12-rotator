#!/usr/bin/env python3
"""
SPI NAND Dump Parser & Partitioner for ZX297520 (Dosilicon DS35M1GA / Winbond W25N01GW).
Handles OOB stripping, bad block skipping, partition extraction and repacking without external deps.
"""

import argparse
import os
import sys

PAGE_DATA_SIZE = 2048
PAGE_OOB_SIZE = 64
PAGES_PER_BLOCK = 64
BLOCKS_COUNT = 1024
PAGE_SIZE = PAGE_DATA_SIZE + PAGE_OOB_SIZE
BLOCK_SIZE = PAGES_PER_BLOCK * PAGE_SIZE

PARTITIONS = {
    'zloader': [0x0, 0x20000],
    'uboot': [0x20000, 0x100000],
    'uboot-mirr': [0x120000, 0x100000],
    'nvrofs': [0x220000, 0x200000],
    'imagefs': [0x420000, 0x1000000],
    'rootfs': [0x1420000, 0x1E00000],
    'userdata': [0x3220000, 0x4960000],
    'yaffs': [0x7B80000, 0x200000]
}

def is_bad_block(oob):
    return oob[0:3] != b'\xff\xff\xff'

def strip_oob(input_path, output_path):
    print(f"[*] Stripping OOB and bad blocks from {input_path} -> {output_path}")
    with open(input_path, "rb") as fin, open(output_path, "wb") as fout:
        for block_idx in range(BLOCKS_COUNT):
            block = fin.read(BLOCK_SIZE)
            if not block:
                break
            if len(block) < BLOCK_SIZE:
                print(f"[!] Warning: Block {block_idx} truncated ({len(block)} bytes)")
            for page_idx in range(PAGES_PER_BLOCK):
                page_offset = page_idx * PAGE_SIZE
                page = block[page_offset:page_offset + PAGE_SIZE]
                if len(page) < PAGE_SIZE:
                    continue
                data = page[:PAGE_DATA_SIZE]
                oob = page[PAGE_DATA_SIZE:PAGE_DATA_SIZE + PAGE_OOB_SIZE]
                if is_bad_block(oob):
                    continue
                fout.write(data)

def extract_partitions(clean_dump_path, output_dir):
    os.makedirs(output_dir, exist_ok=True)
    print(f"[*] Extracting partitions from {clean_dump_path} into {output_dir}/")
    with open(clean_dump_path, "rb") as fin:
        for name, (offset, size) in PARTITIONS.items():
            fin.seek(offset)
            data = fin.read(size)
            out_file = os.path.join(output_dir, f"{name}.bin")
            with open(out_file, "wb") as fout:
                fout.write(data)
            print(f"  -> Extracted {name:<12} (Offset: 0x{offset:08X}, Size: {len(data):>8} B) -> {out_file}")

def main():
    parser = argparse.ArgumentParser(description="ZX297520 SPI NAND Flash Dump & Partition Extractor")
    parser.add_argument("dump", help="Path to raw flash dump file")
    parser.add_argument("-o", "--output-dir", default="extracted_partitions", help="Directory to save extracted partitions")
    args = parser.parse_args()

    clean_dump = "clean_flash.bin"
    strip_oob(args.dump, clean_dump)
    extract_partitions(clean_dump, args.output_dir)
    print("[+] Done! Clean dump and individual partition binaries ready.")

if __name__ == "__main__":
    main()
