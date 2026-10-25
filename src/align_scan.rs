//! Поиск в коде игры мест, где надпись выравнивается по центру или по
//! правому краю (для align.rs). Работает по файлу Dwarf_Fortress.exe,
//! поэтому после обновления игры таблица строится заново сама.
//!
//! Для каждого вызова addst / addst_top:
//!   1. Ищется, где перед ним задан столбец: вызов locate(gps, y, x)
//!      или прямая запись в gps.screenx. Если до этого встретился другой
//!      вызов вывода — надпись продолжает предыдущую, её не трогаем.
//!   2. От столбца прослеживается назад, из чего он посчитан (регистры,
//!      до 30 инструкций, до вызовов функций).
//!   3. Есть «половина» (sar/shr на 1) и вычитание — надпись по центру.
//!      Есть вычитание длины прямо перед locate — по правому краю.
//!   4. Длина взята у литерала (strlen буфера или число, посчитанное
//!      компилятором) — l; у готовой строки (её длина или size) — s.
//!
//! Функции находятся по таблице раскрутки .pdata, разбор — iced-x86.
//! locate находится по своему коду (два mov в [rcx+0x84] и [rcx+0x88]),
//! gps — по lea rcx перед вызовами addst.

use std::collections::{HashMap, HashSet};

use iced_x86::{
  Code, Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind,
  Register,
};

pub struct Site {
  pub center: bool,
  pub ret: u32,
  pub dec: bool,
  pub literal: bool,
}

struct Section {
  name: [u8; 8],
  va: u32,
  vsize: u32,
  raw: u32,
  rsize: u32,
}

struct Pe<'a> {
  data: &'a [u8],
  sections: Vec<Section>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
  Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
  Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

impl<'a> Pe<'a> {
  fn parse(data: &'a [u8]) -> Option<Self> {
    let pe = u32_at(data, 0x3c)? as usize;
    if data.get(pe..pe + 4)? != b"PE\0\0" {
      return None;
    }
    let count = u16_at(data, pe + 6)? as usize;
    let opt = u16_at(data, pe + 20)? as usize;
    let table = pe + 24 + opt;
    let mut sections = Vec::new();
    for i in 0..count {
      let s = table + i * 40;
      let mut name = [0u8; 8];
      name.copy_from_slice(data.get(s..s + 8)?);
      sections.push(Section {
        name,
        vsize: u32_at(data, s + 8)?,
        va: u32_at(data, s + 12)?,
        rsize: u32_at(data, s + 16)?,
        raw: u32_at(data, s + 20)?,
      });
    }
    Some(Self { data, sections })
  }

  fn section(&self, name: &[u8]) -> Option<(u32, &'a [u8])> {
    let s = self.sections.iter().find(|s| s.name.starts_with(name))?;
    let size = s.rsize.min(s.vsize.max(s.rsize)) as usize;
    let bytes = self.data.get(s.raw as usize..s.raw as usize + size)?;
    Some((s.va, bytes))
  }

  /// Отметка времени из заголовка — та же метка версии, что у dfint.
  pub fn stamp(data: &[u8]) -> Option<u32> {
    let pe = u32_at(data, 0x3c)? as usize;
    u32_at(data, pe + 8)
  }
}

pub fn stamp(data: &[u8]) -> Option<u32> {
  Pe::stamp(data)
}

const VOLATILE: [Register; 7] = [
  Register::RAX,
  Register::RCX,
  Register::RDX,
  Register::R8,
  Register::R9,
  Register::R10,
  Register::R11,
];

#[derive(Default)]
struct Feats {
  half: bool,
  sub: bool,
  neg: bool,
  dec: bool,
  strlen: bool,
  size: bool,
  callret: bool,
  nearsub: bool,
}

fn full(r: Register) -> Register {
  r.full_register()
}

/// Из чего посчитан регистр reg перед инструкцией idx.
fn slice(list: &[Instruction], idx: usize, reg: Register, factory: &mut InstructionInfoFactory) -> Feats {
  let mut tracked: HashSet<Register> = HashSet::new();
  tracked.insert(full(reg));
  let mut f = Feats::default();
  let mut steps = 0;
  let mut i = idx;
  while i > 0 && steps < 30 && !tracked.is_empty() {
    i -= 1;
    steps += 1;
    let x = &list[i];
    let mn = x.mnemonic();

    if mn == Mnemonic::Cmp && x.op_count() == 2 && x.op0_kind() == OpKind::Memory && x.memory_size().size() == 1 {
      if matches!(x.op1_kind(), OpKind::Immediate8 | OpKind::Immediate8to32 | OpKind::Immediate8to64)
        && x.immediate(1) == 0
      {
        f.strlen = true;
      }
    }
    if mn == Mnemonic::Call {
      if tracked.contains(&Register::RAX) {
        f.callret = true;
      }
      for r in VOLATILE {
        tracked.remove(&r);
      }
      continue;
    }
    if x.flow_control() != FlowControl::Next {
      continue;
    }

    let info = factory.info(x);
    let mut writes = HashSet::new();
    let mut reads = HashSet::new();
    for u in info.used_registers() {
      let r = full(u.register());
      match u.access() {
        OpAccess::Write | OpAccess::CondWrite => {
          writes.insert(r);
        }
        OpAccess::ReadWrite | OpAccess::ReadCondWrite => {
          writes.insert(r);
          reads.insert(r);
        }
        OpAccess::Read | OpAccess::CondRead => {
          reads.insert(r);
        }
        _ => {}
      }
    }
    let hit: Vec<Register> = writes.iter().copied().filter(|r| tracked.contains(r)).collect();
    if hit.is_empty() {
      continue;
    }

    match mn {
      Mnemonic::Sar | Mnemonic::Shr => {
        if x.op_count() == 2 && x.op1_kind() != OpKind::Register && x.immediate(1) == 1 {
          f.half = true;
        }
      }
      Mnemonic::Sub => f.sub = true,
      Mnemonic::Neg => f.neg = true,
      Mnemonic::Dec => f.dec = true,
      _ => {}
    }

    // Чтение памяти: значение из памяти, регистры адреса — не поток значения
    if mn != Mnemonic::Lea {
      for m in info.used_memory() {
        if matches!(m.access(), OpAccess::Read | OpAccess::CondRead | OpAccess::ReadWrite | OpAccess::ReadCondWrite) {
          if m.displacement() == 0x10 {
            f.size = true;
          }
          reads.remove(&full(m.base()));
          reads.remove(&full(m.index()));
        }
      }
    }
    reads.remove(&Register::RIP);
    reads.remove(&Register::RSP);
    reads.remove(&Register::None);

    let moves = matches!(mn, Mnemonic::Mov | Mnemonic::Movsxd | Mnemonic::Movzx | Mnemonic::Lea | Mnemonic::Movsx);
    let zero = mn == Mnemonic::Xor && x.op_count() == 2 && x.op0_kind() == OpKind::Register
      && x.op1_kind() == OpKind::Register && x.op0_register() == x.op1_register();
    if moves || zero {
      for r in &hit {
        tracked.remove(r);
      }
    }
    // Как в исходном разборе: прочитанные регистры отслеживаются дальше
    tracked.extend(reads);
  }
  f
}

const NEAR_SUB: [Register; 10] = [
  Register::R8D,
  Register::EAX,
  Register::ECX,
  Register::EDX,
  Register::R9D,
  Register::R10D,
  Register::R11D,
  Register::EBX,
  Register::ESI,
  Register::EDI,
];

/// Поиск мест выравнивания. targets — RVA перехваченных функций вывода.
pub fn scan(data: &[u8], targets: &[u32]) -> Option<Vec<Site>> {
  let pe = Pe::parse(data)?;
  let (text_va, text) = pe.section(b".text")?;
  let (_, pdata) = pe.section(b".pdata")?;

  // locate: mov [rcx+0x84], r8d; mov [rcx+0x88], edx; ret
  const LOCATE_CODE: [u8; 14] = [0x44, 0x89, 0x81, 0x84, 0, 0, 0, 0x89, 0x91, 0x88, 0, 0, 0, 0xc3];
  let locate = text.windows(LOCATE_CODE.len()).position(|w| w == LOCATE_CODE)? as u32 + text_va;

  // Функции по таблице раскрутки
  let mut funcs: Vec<(u32, u32)> = pdata
    .chunks_exact(12)
    .map(|c| (u32::from_le_bytes(c[0..4].try_into().unwrap()), u32::from_le_bytes(c[4..8].try_into().unwrap())))
    .filter(|&(b, e)| b < e)
    .collect();
  funcs.sort_unstable();

  // Вызовы вывода
  let mut calls: Vec<(u32, u32)> = Vec::new();
  for i in 0..text.len().saturating_sub(5) {
    if text[i] != 0xe8 {
      continue;
    }
    let rel = i32::from_le_bytes(text[i + 1..i + 5].try_into().unwrap());
    let at = text_va + i as u32;
    let to = (at as i64 + 5 + rel as i64) as u32;
    if targets.contains(&to) {
      calls.push((at, to));
    }
  }

  // gps: lea rcx, [rip+disp] прямо перед вызовом — самый частый адрес
  let mut gps_votes: HashMap<u32, usize> = HashMap::new();
  for &(at, _) in calls.iter().take(400) {
    let o = (at - text_va) as usize;
    if o >= 7 && text[o - 7..o - 4] == [0x48, 0x8d, 0x0d] {
      let disp = i32::from_le_bytes(text[o - 4..o].try_into().unwrap());
      *gps_votes.entry((at as i64 + disp as i64) as u32).or_insert(0) += 1;
    }
  }
  let gps = gps_votes.into_iter().max_by_key(|&(_, n)| n)?.0;
  let screenx = gps as u64 + 0x84;

  let mut factory = InstructionInfoFactory::new();
  let mut cache: HashMap<u32, Vec<Instruction>> = HashMap::new();
  let mut out = Vec::new();

  for &(at, _) in &calls {
    let fi = match funcs.binary_search_by(|&(b, _)| b.cmp(&at)) {
      Ok(i) => i,
      Err(0) => continue,
      Err(i) => i - 1,
    };
    let (fb, fe) = funcs[fi];
    if !(fb <= at && at < fe) {
      continue;
    }
    let list = cache.entry(fb).or_insert_with(|| {
      let start = (fb - text_va) as usize;
      let end = ((fe - text_va) as usize).min(text.len());
      let mut d = Decoder::with_ip(64, &text[start..end], fb as u64, DecoderOptions::NONE);
      let mut v = Vec::new();
      while d.can_decode() {
        let x = d.decode();
        // Как и прежний разбор: дальше первой неразобранной инструкции не идём
        if x.code() == Code::INVALID {
          break;
        }
        v.push(x);
      }
      v
    });
    let Some(idx) = list.iter().position(|x| x.ip() == at as u64) else {
      continue;
    };

    let mut feats = None;
    let mut is_locate = false;
    let lo = idx.saturating_sub(200);
    let mut k = idx;
    while k > lo {
      k -= 1;
      let x = &list[k];
      if x.mnemonic() == Mnemonic::Call && x.op0_kind() == OpKind::NearBranch64 {
        let t = x.near_branch_target() as u32;
        if t == locate {
          let mut f = slice(list, k, Register::R8, &mut factory);
          let from = k.saturating_sub(10);
          f.nearsub = list[from..k].iter().any(|y| {
            y.mnemonic() == Mnemonic::Sub
              && y.op0_kind() == OpKind::Register
              && NEAR_SUB.contains(&y.op0_register())
              && !matches!(
                y.op1_kind(),
                OpKind::Immediate8 | OpKind::Immediate8to32 | OpKind::Immediate32 | OpKind::Immediate8to64 | OpKind::Immediate32to64
              )
          });
          feats = Some(f);
          is_locate = true;
          break;
        }
        if targets.contains(&t) {
          break;
        }
      }
      if x.mnemonic() == Mnemonic::Mov && x.op0_kind() == OpKind::Memory && x.is_ip_rel_memory_operand()
        && x.ip_rel_memory_address() == screenx
      {
        if x.op1_kind() == OpKind::Register {
          feats = Some(slice(list, k, x.op1_register(), &mut factory));
        }
        break;
      }
    }
    let Some(f) = feats else {
      continue;
    };

    let from_string = f.callret || f.size;
    let literal = f.strlen || !from_string;
    if f.half && (f.sub || f.neg) {
      out.push(Site { center: true, ret: at + 5, dec: f.dec, literal });
    } else if is_locate && f.nearsub && (f.size || f.strlen || f.callret) && !f.half {
      out.push(Site { center: false, ret: at + 5, dec: false, literal: f.strlen && !from_string });
    }
  }
  out.sort_by_key(|s| s.ret);
  Some(out)
}

/// Таблица в том же текстовом виде, что и встроенная.
pub fn format(stamp: u32, sites: &[Site]) -> String {
  let mut s = format!("# Dwarf Fortress (PE timestamp 0x{stamp:x}), собрано хуком\n");
  for site in sites {
    s.push_str(&format!(
      "{} {:x} {} {}\n",
      if site.center { 'c' } else { 'r' },
      site.ret,
      site.dec as u8,
      if site.literal { 'l' } else { 's' }
    ));
  }
  s
}
