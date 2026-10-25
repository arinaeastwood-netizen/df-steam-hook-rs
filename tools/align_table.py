"""Сборка таблицы выравнивания надписей (src/align_<метка>.txt) по Dwarf_Fortress.exe.

Запуск: pip install capstone pefile numpy; python align_table.py Dwarf_Fortress.exe
Адреса addst, addst_top и locate — для 52.05 steam win64; для другой версии их
нужно взять из таблицы смещений dfint и найти locate заново (функция из двух mov
в [rcx+0x84] и [rcx+0x88]), а GPS — по любому вызову addst (lea rcx, [rip+...]).
"""
import sys
import pefile, struct, re, sys
pe=pefile.PE(sys.argv[1], fast_load=True)
data=open(sys.argv[1],'rb').read()
IB=0x140000000
secs={s.Name.rstrip(b'\0').decode():s for s in pe.sections}
def off2rva(off):
    for s in pe.sections:
        if s.PointerToRawData<=off<s.PointerToRawData+s.SizeOfRawData: return off-s.PointerToRawData+s.VirtualAddress
def rva2off(rva):
    for s in pe.sections:
        if s.VirtualAddress<=rva<s.VirtualAddress+s.SizeOfRawData: return rva-s.VirtualAddress+s.PointerToRawData
text=secs['.text']; tbase=text.VirtualAddress; tdata=data[text.PointerToRawData:text.PointerToRawData+text.SizeOfRawData]
def xrefs(target_rva):
    # find RIP-relative references (lea/mov with disp32) : disp = target - (rva_of_next_insn)
    out=[]
    # brute force: scan for 4-byte disp where insn length 7 (48 8d 05/0d/15/1d/...); check opcode patterns
    for m in re.finditer(rb'[\x48\x4c][\x8d\x8b][\x05\x0d\x15\x1d\x25\x2d\x35\x3d]', tdata):
        i=m.start()
        disp=struct.unpack_from('<i',tdata,i+3)[0]
        if tbase+i+7+disp==target_rva: out.append(tbase+i)
    return out
import numpy as np
_arr=None
def anyxrefs(target_rva, tails=(0,1,4)):
    global _arr
    if _arr is None:
        n=len(tdata)-4
        _arr=np.frombuffer(tdata[:n+3],dtype=np.uint8)
    b=np.frombuffer(tdata,dtype=np.uint8).astype(np.int64)
    d=(b[:-3] | (b[1:-2]<<8) | (b[2:-1]<<16) | (b[3:]<<24))
    d=np.where(d>=2**31, d-2**32, d)
    idx=np.arange(len(d))
    out=[]
    for t in tails:
        hits=np.nonzero(tbase+idx+4+t+d==target_rva)[0]
        out+= [int(tbase+h) for h in hits]
    return sorted(set(out))
def calls_to(target):
    out=[]
    for m in re.finditer(rb'\xe8', tdata):
        i=m.start()
        if i+5>len(tdata): break
        d=struct.unpack_from('<i',tdata,i+1)[0]
        if tbase+i+5+d==target: out.append(tbase+i)
    return out
from capstone import *
from capstone.x86 import *
import bisect, collections
md=Cs(CS_ARCH_X86, CS_MODE_64); md.detail=True
pd=secs['.pdata']; pdata=data[pd.PointerToRawData:pd.PointerToRawData+pd.Misc_VirtualSize]
funcs=sorted(struct.unpack_from('<III',pdata,i)[:2] for i in range(0,len(pdata)-11,12))
fstarts=[f[0] for f in funcs]
def func_of(rva):
    i=bisect.bisect_right(fstarts,rva)-1
    if i<0: return None
    b,e=funcs[i]
    return (b,e) if b<=rva<e else None
GPS=0x255d11+0x235c9df
LOCATE=0xc0710
def regname(r): return md.reg_name(r)
FAM={}
for base in ['rax','rbx','rcx','rdx','rsi','rdi','rbp','rsp']+['r%d'%i for i in range(8,16)]:
    pass
def fam(name):
    if name is None: return None
    n=name
    m={'eax':'rax','ax':'rax','al':'rax','ah':'rax','ebx':'rbx','bx':'rbx','bl':'rbx','ecx':'rcx','cx':'rcx','cl':'rcx','edx':'rdx','dx':'rdx','dl':'rdx',
       'esi':'rsi','si':'rsi','sil':'rsi','edi':'rdi','di':'rdi','dil':'rdi','ebp':'rbp','bp':'rbp','esp':'rsp'}
    if n in m: return m[n]
    mm=re.match(r'(r\d+)[dwb]?$',n)
    if mm: return mm.group(1)
    return n
VOLATILE={'rax','rcx','rdx','r8','r9','r10','r11'}
_cache={}
def insns(b,e):
    key=(b,e)
    if key not in _cache:
        _cache[key]=list(md.disasm(tdata[b-tbase:e-tbase], b))
    return _cache[key]
def slice_x(ins_list, idx, reg='r8', depth=30):
    """Backward slice for register reg before instruction idx. Returns set of features."""
    tracked={reg}; feats=set(); steps=0
    mem_tracked=set()
    i=idx-1
    while i>=0 and steps<depth and tracked:
        x=ins_list[i]; steps+=1
        if x.mnemonic=='call':
            hit=tracked & VOLATILE
            if 'rax' in hit: feats.add('callret')
            tracked-=VOLATILE
            i-=1; continue
        if x.mnemonic.startswith('j') or x.mnemonic=='ret':
            i-=1; continue
        regs_read, regs_write = x.regs_access()
        w={fam(regname(r)) for r in regs_write}-{'rflags'}
        # memory store to tracked stack slot
        ops=x.operands
        writes_tracked = w & tracked
        # stack slot tracking: if instruction loads tracked reg from [rsp/rbp+disp], track that slot
        if writes_tracked:
            mn=x.mnemonic
            if mn in ('sar','shr') and len(ops)==2 and ops[1].type==X86_OP_IMM and ops[1].imm==1: feats.add('half')
            if mn=='cdq': feats.add('cdq')
            if mn=='sub':
                feats.add('sub')
                feats.add('subimm' if ops[1].type==X86_OP_IMM else 'subreg')
            if mn=='neg': feats.add('neg')
            if mn in ('dec',): feats.add('dec')
            r={fam(regname(r)) for r in regs_read}-{'rflags','rip','rsp'}
            memread=False
            for op in ops:
                if op.type==X86_OP_MEM and (op.access & CS_AC_READ) and mn!='lea':
                    memread=True
                    feats.add('mem')
                    if op.mem.disp==0x10: feats.add('size')
                    # регистры адреса — не поток значения
                    for rr in (op.mem.base, op.mem.index):
                        if rr: r.discard(fam(regname(rr)))
            if mn in ('mov','movsxd','movzx','lea','movsx'):
                tracked-= writes_tracked
            if mn=='xor' and len(ops)==2 and ops[0].type==X86_OP_REG and ops[1].type==X86_OP_REG and ops[0].reg==ops[1].reg:
                tracked-=writes_tracked
            tracked|= r
        # strlen loop pattern: cmp byte ptr [base+rax],0
        if x.mnemonic=='cmp' and 'byte ptr' in x.op_str and x.op_str.endswith(', 0'): feats.add('strlen')
        i-=1
    return feats
def classify_site(c):
    f=func_of(c)
    if not f: return None
    L=insns(f[0],f[1])
    idx=next((k for k,x in enumerate(L) if x.address==c),None)
    if idx is None: return None
    # find previous locate call (no other addst between)
    for k in range(idx-1,max(idx-120,-1),-1):
        x=L[k]
        if x.mnemonic=='call' and x.operands[0].type==X86_OP_IMM:
            t=x.operands[0].imm
            if t==LOCATE:
                return slice_x(L,k,'r8'), L[k].address
            if t in ADDS: return None
    return None
ADDS={0xa994f0}
SX=GPS+0x84
def writes_sx(x):
    if x.mnemonic not in ('mov',): return None
    ops=x.operands
    if len(ops)==2 and ops[0].type==X86_OP_MEM and ops[0].mem.base==X86_REG_RIP:
        if x.address+x.size+ops[0].mem.disp==SX:
            return ops[1]
    return None
def classify_site2(c, window=200):
    f=func_of(c)
    if not f: return ('nofunc',None)
    L=insns(f[0],f[1])
    idx=next((k for k,x in enumerate(L) if x.address==c),None)
    if idx is None: return ('noidx',None)
    for k in range(idx-1,max(idx-window,-1),-1):
        x=L[k]
        if x.mnemonic=='call' and x.operands and x.operands[0].type==X86_OP_IMM:
            t=x.operands[0].imm
            if t==LOCATE:
                f=slice_x(L,k,'r8')
                near=L[max(0,k-10):k]
                if any(y.mnemonic=='sub' and y.op_str.split(',')[0].strip() in ('r8d','eax','ecx','edx','r9d','r10d','r11d','ebx','esi','edi') and not y.op_str.split(',')[1].strip().startswith('0x') for y in near):
                    f.add('nearsub')
                return ('locate', f)
            if t in ADDS: return ('chain',None)
        op=writes_sx(x)
        if op is not None:
            if op.type==X86_OP_REG: return ('inline', slice_x(L,k,fam(regname(op.reg))))
            return ('inline-imm', set())
    return ('none',None)
ADDS.add(0xa995d0)
import collections, json
rows=[]; cnt=collections.Counter()
for fn in (0xa994f0,0xa995d0):
    for c in calls_to(fn):
        kind,f=classify_site2(c)
        if f is None or kind=='inline-imm': continue
        strsrc=bool({'callret','size'} & f)
        lit= ('strlen' in f) or (not strsrc)
        if 'half' in f and ('sub' in f or 'neg' in f):
            rows.append(('c',c+5,int('dec' in f), 'l' if lit else 's')); cnt['c'+('l' if lit else 's')]+=1
        elif kind=='locate' and 'nearsub' in f and ({'size','strlen','callret'} & f) and 'half' not in f:
            rows.append(('r',c+5,0,'l' if 'strlen' in f and not strsrc else 's')); cnt['r'+rows[-1][3]]+=1
print(cnt, len(rows))
with open('align_%x.txt' % pe.FILE_HEADER.TimeDateStamp,'w') as fo:
    fo.write('# Dwarf Fortress (PE timestamp 0x%x)\n' % pe.FILE_HEADER.TimeDateStamp)
    fo.write('# вид адрес_возврата(RVA) вычитание_единицы источник_длины\n')
    fo.write('# c — по центру, r — по правому краю; l — длина английского литерала, s — длина строки\n')
    for k,r,d,src in sorted(rows,key=lambda x:x[1]): fo.write(f'{k} {r:x} {d} {src}\n')
