//! Кнопки фиксированной ширины: рамка под длину перевода.
//!
//! Часть кнопок игра рисует своим циклом с шириной, вписанной в код
//! числом: рамка из трёх частей (левый край, середина, правый край) и
//! надпись-константа на клетку правее левого края. В 52.05 так устроены
//! кнопки оповещений слева: DIPLOMACY и PETITIONS (11 клеток, внутри
//! 9 букв), SIEGE и ALERT (7 клеток, внутри 5 букв). «ДИПЛОМАТИЯ» и
//! «Тревога» в такую рамку не помещаются, и перевод до раскладки тут не
//! поможет: ширина нигде не считается, она просто записана в коде.
//!
//! Хук сам находит такие кнопки в коде игры по их устройству:
//!
//!   cmp R, N / jne / mov edx,[P+0x18] / jmp / test R,R / jne /
//!   mov edx,[P] / jmp / mov edx,[P+0xc]   — выбор части рамки;
//!   inc R / cmp R, N / jle                  — конец цикла по клеткам;
//!   lea rdx,[надпись] … call addst          — надпись кнопки.
//!
//! N — номер последней клетки (ширина − 1). Для каждой кнопки хук берёт
//! перевод надписи из словарей и, если он не помещается, увеличивает N в
//! обоих сравнениях, — рамка становится шире, надпись остаётся на месте.
//! Сужать хук ничего не будет: меньше исходной ширины N не бывает.
//! Перевод может поменяться (ручная правка, перезагрузка словаря), поэтому
//! ширина пересчитывается при каждой перезагрузке словарей — в основном
//! потоке игры, перед выводом очередной надписи.
//!
//! Ограничение: место нажатия мышью игра проверяет в другом участке кода,
//! и добавленная часть рамки на щелчок может не отзываться.
//!
//! Не нашлось ни одной кнопки (другая версия игры) — ничего не меняется.
//! Аварийный выключатель — строка buttons_off в dfint-data/ai/experiments.txt.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

/// Насколько хук может расширить рамку, клеток.
const MAX_EXTRA: u8 = 12;

struct Button {
  /// Адреса числа N в двух сравнениях.
  imm: [usize; 2],
  /// Исходное N.
  orig: u8,
  /// N сейчас.
  current: u8,
  /// Надпись кнопки (английская константа).
  label: Vec<u8>,
}

static BUTTONS: LazyLock<Mutex<Vec<Button>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static READY: AtomicBool = AtomicBool::new(false);
/// Версия словарей, под которую рамки уже подогнаны (usize::MAX — ещё ни разу).
static APPLIED: AtomicUsize = AtomicUsize::new(usize::MAX);

#[cfg(target_os = "windows")]
mod os {
  #[link(name = "kernel32")]
  extern "system" {
    fn VirtualProtect(address: *mut core::ffi::c_void, size: usize, protect: u32, old: *mut u32) -> i32;
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn FlushInstructionCache(process: *mut core::ffi::c_void, address: *const core::ffi::c_void, size: usize) -> i32;
  }
  const PAGE_EXECUTE_READWRITE: u32 = 0x40;

  /// Один байт кода игры.
  pub unsafe fn write_byte(address: usize, value: u8) -> bool {
    let mut old = 0u32;
    if VirtualProtect(address as *mut _, 1, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
      return false;
    }
    std::ptr::write_volatile(address as *mut u8, value);
    let mut ignored = 0u32;
    VirtualProtect(address as *mut _, 1, old, &mut ignored);
    FlushInstructionCache(GetCurrentProcess(), address as *const _, 1);
    true
  }
}

#[cfg(target_os = "windows")]
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
  let table = nt + 24 + rd16(nt + 20);
  for i in 0..sections {
    let s = table + i * 40;
    let name = std::slice::from_raw_parts((base + s) as *const u8, 8);
    if name.starts_with(b".text") {
      return Some((rd32(s + 12), rd32(s + 8)));
    }
  }
  None
}

/// Надпись по адресу внутри образа игры: печатная ASCII-строка 2–40 знаков.
#[cfg(target_os = "windows")]
unsafe fn label_at(address: usize, base: usize) -> Option<Vec<u8>> {
  let nt = std::ptr::read_unaligned((base + 0x3c) as *const u32) as usize;
  let image = std::ptr::read_unaligned((base + nt + 0x50) as *const u32) as usize;
  if address < base || address + 41 > base + image {
    return None;
  }
  let bytes = std::slice::from_raw_parts(address as *const u8, 41);
  let len = bytes.iter().position(|&b| b == 0)?;
  let label = &bytes[..len];
  if len < 2 || !label.iter().all(|&b| (32..127).contains(&b)) || !label.iter().any(|b| b.is_ascii_alphabetic()) {
    return None;
  }
  Some(label.to_vec())
}

/// Поиск кнопок в коде игры.
#[cfg(target_os = "windows")]
unsafe fn scan() -> Vec<Button> {
  let base = *crate::utils::MODULE;
  let Some(addst) = crate::config::CONFIG.offset.as_ref().and_then(|o| o.addst) else {
    log::warn!("buttons: no addst offset, buttons are left as is");
    return Vec::new();
  };
  let addst = base + addst;
  let Some((rva, size)) = text_section(base) else {
    return Vec::new();
  };
  let code = std::slice::from_raw_parts((base + rva) as *const u8, size);
  let at = |i: usize| base + rva + i;

  let frame = regex::bytes::Regex::new(
    r"(?s-u)\x83[\xF8-\xFF].\x75\x05\x8B.\x18\xEB\x0B\x85.\x75\x04\x8B.\xEB\x03\x8B.\x0C",
  )
  .unwrap();
  let mut out = Vec::new();
  for m in frame.find_iter(code) {
    let i = m.start();
    let reg = code[i + 1] & 7;
    let n = code[i + 2];
    // test R, R — тот же регистр; ширина правдоподобная
    if code[i + 11] != (0xC0 | (reg << 3) | reg) || !(2..=40).contains(&n) {
      continue;
    }
    // inc R / cmp R, N / jle назад — конец цикла по клеткам
    let second = (i + 20..(i + 0x70).min(code.len() - 7)).find(|&j| {
      code[j] == 0xFF
        && code[j + 1] == (0xC0 | reg)
        && code[j + 2] == 0x83
        && code[j + 3] == (0xF8 | reg)
        && code[j + 4] == n
        && code[j + 5] == 0x7E
        && (code[j + 6] as i8) < 0
    });
    let Some(second) = second else { continue };
    // lea rdx, [надпись] и вызов addst вскоре после него
    let mut label = None;
    for k in second..(second + 0x90).min(code.len() - 7) {
      if code[k..k + 3] != [0x48, 0x8D, 0x15] {
        continue;
      }
      let rel = i32::from_le_bytes(code[k + 3..k + 7].try_into().unwrap()) as isize;
      let target = (at(k + 7) as isize + rel) as usize;
      let Some(text) = label_at(target, base) else { continue };
      let calls_addst = (k + 7..(k + 0x40).min(code.len() - 5)).any(|c| {
        code[c] == 0xE8 && {
          let rel = i32::from_le_bytes(code[c + 1..c + 5].try_into().unwrap()) as isize;
          (at(c + 5) as isize + rel) as usize == addst
        }
      });
      if calls_addst {
        label = Some(text);
        break;
      }
    }
    let Some(label) = label else { continue };
    out.push(Button {
      imm: [at(i + 2), at(second + 4)],
      orig: n,
      current: n,
      label,
    });
  }
  out
}

/// Найти кнопки. Вызывается при запуске.
pub unsafe fn attach() {
  if crate::ai::EXPERIMENTS.buttons_off {
    return;
  }
  #[cfg(target_os = "windows")]
  {
    let found = scan();
    let base = *crate::utils::MODULE;
    for b in &found {
      log::info!(
        "buttons: {} at 0x{:x}, width {}",
        String::from_utf8_lossy(&b.label),
        b.imm[0] - base,
        b.orig as usize + 1
      );
    }
    if found.is_empty() {
      log::info!("buttons: no fixed-width buttons found");
      return;
    }
    *BUTTONS.lock().unwrap() = found;
    READY.store(true, Ordering::Relaxed);
  }
}

/// Длина самого длинного перевода надписи (байт в кодировке игры).
fn translated_len(label: &[u8]) -> Option<usize> {
  let strip = |v: Vec<u8>| {
    let end = v.iter().rposition(|&b| b != 0 && b != b' ').map_or(0, |i| i + 1);
    let start = v.iter().position(|&b| b != b' ').unwrap_or(0).min(end);
    end - start
  };
  [crate::ai::peek(label), crate::ai::lookup_ui(label), crate::dictionary::lookup(label)]
    .into_iter()
    .flatten()
    .map(strip)
    .filter(|&n| n > 0)
    .max()
}

/// Подогнать рамки под переводы. Дёшево, если словари не менялись.
/// Вызывается из вывода надписей — в основном потоке игры, тем же, что
/// рисует кнопки.
pub fn refresh() {
  if !READY.load(Ordering::Relaxed) {
    return;
  }
  let generation = crate::ai::generation();
  if APPLIED.load(Ordering::Relaxed) == generation {
    return;
  }
  let Ok(mut buttons) = BUTTONS.try_lock() else { return };
  APPLIED.store(generation, Ordering::Relaxed);
  for b in buttons.iter_mut() {
    // Перевода не видно — словарь как раз перезагружается (он занят, и
    // поиск в нём на миг ничего не находит). Рамка остаётся как есть:
    // иначе она сужалась и тут же расширялась обратно при каждой загрузке
    let Some(len) = translated_len(&b.label) else { continue };
    // Надпись стоит с клетки 1, правый край — клетка N: N ≥ длина + 1
    let want = (len + 1).clamp(b.orig as usize, (b.orig + MAX_EXTRA) as usize) as u8;
    if want == b.current {
      continue;
    }
    #[cfg(target_os = "windows")]
    unsafe {
      if os::write_byte(b.imm[0], want) && os::write_byte(b.imm[1], want) {
        log::info!(
          "buttons: {} width {} -> {}",
          String::from_utf8_lossy(&b.label),
          b.current as usize + 1,
          want as usize + 1
        );
        b.current = want;
      }
    }
  }
}
