//! Перевод текста до раскладки: функция переноса строк игры.
//!
//! Игра раскладывает экран под английский текст, а перевод раньше
//! подставлялся только при выводе букв — когда окно уже нарисовано под
//! английскую длину. Русский текст длиннее и вылезал за рамки.
//!
//! Длинный текст — оповещения, описания, подсказки, справку — игра
//! переносит по словам одной общей функцией (52.05: 0x8b9f10, её зовут
//! больше тысячи мест). Ей передаются: куда сложить строки
//! (std::vector<std::string*>), текст (const std::string&) и ширина.
//! Строки она складывает новыми std::string, а исходный текст только
//! читает. По числу строк игра потом считает высоту окна.
//!
//! Хук перехватывает эту функцию. Если для текста есть перевод, игре
//! вместо английского текста передаётся временная русская копия: игра
//! сама переносит русский текст по своей ширине, окно вырастает на
//! нужное число строк, и ничего не вылезает. Сам текст в данных игры
//! (оповещение, описание) остаётся английским — переведённого нечему
//! попасть в сохранение.
//!
//! Если перевода ещё нет, текст целиком уходит в журнал (запись T), а
//! игра переносит английский текст как обычно. Его строки при выводе
//! по-прежнему ищутся в словаре (старые переводы абзацев работают), но
//! в журнал отдельно не пишутся: весь текст уже там.
//!
//! Счётчик повторов оповещения (« x6» в конце) не входит в ключ: один
//! перевод на все повторы, счётчик дописывается к переводу.
//!
//! Функция находится в коде игры по её байтам, как поиск подстроки в
//! search.rs: адресов в offsets.toml dfint для неё нет, и после
//! обновления игры она находится сама. Не нашлась или нашлась дважды —
//! перехват не ставится, всё работает как раньше.
//!
//! Работает только в режиме оригинального текста. Аварийный выключатель —
//! строка wrap_off в dfint-data/ai/experiments.txt.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::ai;

/// Перехват поставлен.
static ATTACHED: AtomicBool = AtomicBool::new(false);
/// Перехват поставлен и включён.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Предел запоминаемых строк. Больше — набор начинается заново.
const MAX_LINES: usize = 20_000;
/// Длиннее этого текст не переводится и не пишется в журнал.
const MAX_TEXT: usize = 4000;

/// Строки, которые сложила игра из русского текста: при выводе их
/// переводить не нужно, и в журнал они не идут.
static OUTPUT: LazyLock<Mutex<HashSet<Vec<u8>>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Строки английского текста, который уже ушёл в журнал целиком: при
/// выводе они ищутся в словаре, но в журнал отдельно не пишутся.
static PENDING: LazyLock<Mutex<HashSet<Vec<u8>>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

fn trim(s: &[u8]) -> &[u8] {
  let start = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
  let end = s.iter().rposition(|&b| b != b' ').map_or(start, |i| i + 1);
  &s[start..end.max(start)]
}

fn contains(set: &Mutex<HashSet<Vec<u8>>>, key: &[u8]) -> bool {
  match set.try_lock() {
    Ok(set) => !set.is_empty() && (set.contains(key) || set.contains(trim(key))),
    Err(_) => false,
  }
}

/// Строка — из русского текста, перенесённого игрой: выводится как есть.
pub fn is_output(key: &[u8]) -> bool {
  ACTIVE.load(Ordering::Relaxed) && key.iter().any(|&b| b >= 0x80) && contains(&OUTPUT, key)
}

/// Строка английского текста, который весь уже в журнале.
pub fn is_pending(key: &[u8]) -> bool {
  ACTIVE.load(Ordering::Relaxed) && contains(&PENDING, key)
}

/// Счётчик повторов на конце: «… Needs potash. x6» → («… Needs potash.», « x6»).
fn split_counter(text: &[u8]) -> (&[u8], &[u8]) {
  let n = text.len();
  let digits = text.iter().rev().take_while(|b| b.is_ascii_digit()).count();
  if digits == 0 || digits > 5 || n < digits + 3 {
    return (text, &[]);
  }
  let at = n - digits;
  if text[at - 1] != b'x' || text[at - 2] != b' ' {
    return (text, &[]);
  }
  (&text[..at - 2], &text[at - 2..])
}

/// Стоит ли текст перевода: латиница (в режиме оригинального текста
/// русских кусков в нём нет) и хотя бы два слова.
fn worth(text: &[u8]) -> bool {
  if text.len() < 4 || text.len() > MAX_TEXT {
    return false;
  }
  let letters = text.iter().filter(|b| b.is_ascii_alphabetic()).count();
  letters >= 3 && trim(text).contains(&b' ')
}

/// Строки, добавленные игрой в вектор после индекса from, — в набор.
unsafe fn remember(dest: usize, from: usize, set: &Mutex<HashSet<Vec<u8>>>) {
  let Some(lines) = read_lines(dest, from) else { return };
  let Ok(mut set) = set.try_lock() else { return };
  if set.len() + lines.len() * 2 > MAX_LINES {
    set.clear();
  }
  for line in lines {
    let trimmed = trim(&line).to_vec();
    if trimmed.is_empty() {
      continue;
    }
    if trimmed.len() != line.len() {
      set.insert(trimmed);
    }
    set.insert(line);
  }
}

/// Число строк в std::vector<std::string*> (начало и конец — первые два поля).
unsafe fn vec_len(dest: usize) -> Option<usize> {
  let begin = std::ptr::read_unaligned(dest as *const usize);
  let end = std::ptr::read_unaligned((dest + 8) as *const usize);
  if begin == 0 && end == 0 {
    return Some(0);
  }
  if begin == 0 || end < begin || (end - begin) % 8 != 0 || (end - begin) / 8 > 100_000 {
    return None;
  }
  Some((end - begin) / 8)
}

unsafe fn read_lines(dest: usize, from: usize) -> Option<Vec<Vec<u8>>> {
  let n = vec_len(dest)?;
  if n <= from {
    return Some(Vec::new());
  }
  let begin = std::ptr::read_unaligned(dest as *const usize);
  let mut out = Vec::with_capacity(n - from);
  for i in from..n {
    let p = std::ptr::read_unaligned((begin + i * 8) as *const usize);
    if p == 0 {
      continue;
    }
    let s = crate::cxxstring::CxxString::from_ptr(p as *const u8);
    if s.len > MAX_TEXT {
      continue;
    }
    out.push(s.to_bytes_without_nul().to_vec());
  }
  Some(out)
}

/// Сколько первых переводов и промахов пишется в лог.
const LOG_LIMIT: usize = 20;
static LOGGED_HIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static LOGGED_MISS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(target_os = "windows")]
mod hook {
  use super::*;
  use crate::cxxstring::{CxxString, CxxStringContent};
  use retour::static_detour;

  static_detour! {
    static WRAP: unsafe extern "C" fn(usize, *const u8, i32) -> usize;
  }

  /// Перенос строк из 52.05 (0x8b9f10): проверка ширины, копия текста в
  /// новую std::string, вектор из одной строки и вызов переноса абзацев.
  /// ?? — смещения переходов и адреса вызовов: меняются от сборки к сборке.
  const PATTERN: &str = "45 85 C0 0F 8E ?? ?? ?? ?? 48 89 5C 24 08 48 89 6C 24 10 56 57 41 56 \
    48 83 EC 40 41 8B F8 48 8B DA 48 8B F1 0F 57 C0 F3 0F 7F 44 24 20 33 ED 48 89 6C 24 30 \
    B9 20 00 00 00 E8 ?? ?? ?? ?? 0F 57 C0 0F 11 00 48 89 68 10 48 C7 40 18 0F 00 00 00 40 88 28 \
    48 89 44 24 78 48 3B C3 74 ?? 4C 8B 43 10 48 83 7B 18 0F 76 ?? 48 8B 1B 48 8B D3 48 8B C8 \
    E8 ?? ?? ?? ?? 4C 8D 44 24 78 33 D2 48 8D 4C 24 20 E8 ?? ?? ?? ?? 44 8B C7 48 8D 54 24 20 \
    48 8B CE E8 ?? ?? ?? ??";

  fn parse_pattern() -> Vec<Option<u8>> {
    PATTERN
      .split_whitespace()
      .map(|t| if t == "??" { None } else { u8::from_str_radix(t, 16).ok() })
      .collect()
  }

  unsafe fn text_section(base: usize) -> Option<(usize, usize)> {
    let rd16 = |o: usize| std::ptr::read_unaligned((base + o) as *const u16) as usize;
    let rd32 = |o: usize| std::ptr::read_unaligned((base + o) as *const u32) as usize;
    if rd16(0) != 0x5a4d {
      return None;
    }
    let nt = rd32(0x3c);
    if rd32(nt) != 0x4550 {
      return None;
    }
    let sections = rd16(nt + 6);
    let optional = rd16(nt + 20);
    let table = nt + 24 + optional;
    for i in 0..sections {
      let s = table + i * 40;
      let name = std::slice::from_raw_parts((base + s) as *const u8, 8);
      if name.starts_with(b".text") {
        return Some((rd32(s + 12), rd32(s + 8)));
      }
    }
    None
  }

  unsafe fn locate() -> Option<usize> {
    let base = *crate::utils::MODULE;
    let (rva, size) = text_section(base)?;
    let code = std::slice::from_raw_parts((base + rva) as *const u8, size);
    let pattern = parse_pattern();
    let head: Vec<u8> = pattern.iter().take_while(|b| b.is_some()).map(|b| b.unwrap()).collect();
    if head.len() < 3 || pattern.len() < 100 {
      return None;
    }
    let mut found = Vec::new();
    let mut i = 0;
    while i + pattern.len() <= code.len() {
      if code[i] == head[0] && code[i..i + head.len()] == head[..] {
        let all = pattern.iter().enumerate().all(|(k, p)| p.map_or(true, |b| code[i + k] == b));
        if all {
          found.push(base + rva + i);
          if found.len() > 1 {
            break;
          }
        }
      }
      i += 1;
    }
    match found.as_slice() {
      [one] => Some(*one),
      [] => {
        log::warn!("wrap: line wrapping function not found, long text is translated at output as before");
        None
      }
      _ => {
        log::warn!("wrap: line wrapping function found twice, hook not installed");
        None
      }
    }
  }

  /// Временная std::string (раскладка MSVC) поверх своего буфера. Игра
  /// её только читает: функция переноса копирует текст сама.
  struct TempString {
    _buf: Vec<u8>,
    s: CxxString,
  }

  fn temp_string(text: &[u8]) -> TempString {
    let len = text.len();
    let mut buf = Vec::with_capacity(len.max(16) + 1);
    buf.extend_from_slice(text);
    buf.resize(len.max(16) + 1, 0);
    let s = if len >= 16 {
      CxxString {
        data: CxxStringContent { ptr: buf.as_mut_ptr() },
        len,
        capa: len,
      }
    } else {
      let mut small = [0u8; 16];
      small.copy_from_slice(&buf[..16]);
      CxxString {
        data: CxxStringContent { buf: small },
        len,
        capa: 15,
      }
    };
    TempString { _buf: buf, s }
  }

  fn detour(dest: usize, text: *const u8, width: i32) -> usize {
    unsafe {
      if dest == 0 || text.is_null() || width <= 0 || !ACTIVE.load(Ordering::Relaxed) {
        return WRAP.call(dest, text, width);
      }
      let source = CxxString::from_ptr(text);
      if source.len > MAX_TEXT {
        return WRAP.call(dest, text, width);
      }
      let bytes = source.to_bytes_without_nul().to_vec();
      if !worth(&bytes) {
        return WRAP.call(dest, text, width);
      }
      let (base, counter) = split_counter(&bytes);
      let before = vec_len(dest);

      match ai::lookup(base) {
        Some(mut value) => {
          if value.last() == Some(&0) {
            value.pop();
          }
          if value.is_empty() {
            return WRAP.call(dest, text, width);
          }
          value.extend_from_slice(counter);
          if LOGGED_HIT.fetch_add(1, Ordering::Relaxed) < LOG_LIMIT {
            log::info!("wrap: translated before layout, width {width}, {} -> {} bytes", bytes.len(), value.len());
          }
          let tmp = temp_string(&value);
          let ret = WRAP.call(dest, &tmp.s as *const CxxString as *const u8, width);
          if let Some(from) = before {
            remember(dest, from, &OUTPUT);
          }
          ret
        }
        None => {
          if ai::send_entry(b'T', base.to_vec()) && LOGGED_MISS.fetch_add(1, Ordering::Relaxed) < LOG_LIMIT {
            log::info!("wrap: new text for translation, width {width}, {} bytes", base.len());
          }
          let ret = WRAP.call(dest, text, width);
          if let Some(from) = before {
            remember(dest, from, &PENDING);
          }
          ret
        }
      }
    }
  }

  pub unsafe fn attach() {
    let Some(address) = locate() else {
      return;
    };
    let target: unsafe extern "C" fn(usize, *const u8, i32) -> usize = std::mem::transmute(address);
    match WRAP.initialize(target, detour).and_then(|d| d.enable()) {
      Ok(_) => {
        ATTACHED.store(true, Ordering::Relaxed);
        ACTIVE.store(true, Ordering::Relaxed);
        log::info!("wrap: line wrapping hooked at 0x{:x}", address - *crate::utils::MODULE);
      }
      Err(err) => log::error!("wrap: unable to hook line wrapping: {err:?}"),
    }
  }

  pub unsafe fn set_enabled(on: bool) {
    if !ATTACHED.load(Ordering::Relaxed) {
      return;
    }
    if WRAP.is_enabled() != on {
      let _ = if on { WRAP.enable() } else { WRAP.disable() };
    }
    ACTIVE.store(on, Ordering::Relaxed);
  }
}

/// Перехват переноса строк. Только в режиме оригинального текста.
pub unsafe fn attach() {
  if !ai::EXPERIMENTS.english || ai::EXPERIMENTS.wrap_off {
    return;
  }
  #[cfg(target_os = "windows")]
  hook::attach();
}

/// Включение и выключение вместе с остальным переводом (Ctrl+F2).
#[allow(dead_code)]
pub unsafe fn set_enabled(on: bool) {
  #[cfg(target_os = "windows")]
  hook::set_enabled(on);
  #[cfg(not(target_os = "windows"))]
  let _ = on;
}
