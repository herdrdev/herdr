import struct
import sys

def patch_elf(filename):
    with open(filename, 'r+b') as f:
        data = f.read(64)
        if len(data) < 64 or data[:4] != b'\x7fELF':
            print("Not a valid ELF file")
            return
        
        # 64-bit ELF is class 2
        if data[4] != 2:
            print("Not a 64-bit ELF file")
            return
        
        # Read e_phoff (offset 32), e_phentsize (offset 54), e_phnum (offset 56)
        e_phoff = struct.unpack('<Q', data[32:40])[0]
        e_phentsize = struct.unpack('<H', data[54:56])[0]
        e_phnum = struct.unpack('<H', data[56:58])[0]
        
        print(f"e_phoff: {e_phoff}, e_phentsize: {e_phentsize}, e_phnum: {e_phnum}")
        
        for i in range(e_phnum):
            entry_offset = e_phoff + i * e_phentsize
            f.seek(entry_offset)
            entry_data = f.read(e_phentsize)
            if len(entry_data) < 56:
                break
            
            p_type = struct.unpack('<I', entry_data[:4])[0]
            if p_type == 7: # PT_TLS
                p_offset = struct.unpack('<Q', entry_data[8:16])[0]
                p_vaddr = struct.unpack('<Q', entry_data[16:24])[0]
                p_align = struct.unpack('<Q', entry_data[48:56])[0]
                print(f"Found PT_TLS entry at index {i} with offset {hex(p_offset)}, vaddr {hex(p_vaddr)}, alignment {p_align}")
                
                new_align = max(p_align, 64)
                skew = p_vaddr % new_align
                new_vaddr = p_vaddr - skew
                new_offset = p_offset - skew
                
                f.seek(entry_offset + 8)
                f.write(struct.pack('<Q', new_offset))
                f.seek(entry_offset + 16)
                f.write(struct.pack('<Q', new_vaddr))
                f.seek(entry_offset + 48)
                f.write(struct.pack('<Q', new_align))
                print(f"Patched PT_TLS: offset -> {hex(new_offset)}, vaddr -> {hex(new_vaddr)}, alignment -> {new_align}")

if __name__ == '__main__':
    if len(sys.argv) < 2:
        print("Usage: patch_tls.py <elf-file>")
        sys.exit(1)
    patch_elf(sys.argv[1])
