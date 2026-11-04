//! Окна под русский текст: ширина окна дварфа растёт, а не текст режется.
//!
//! Окно дварфа (52.05) игра рисует в одной огромной функции (0x14c8d0).
//! Его прямоугольник считается так (диагностика frame-src):
//!
//!   правый край = [rsp+0xae08] − 0xd − 0x18
//!   левый край  = правый край − 0x3c        ← ширина окна, 61 клетка
//!
//! Окно растёт ВЛЕВО, на карту: ширина увеличивается на N клеток
//! (0x3c → 0x3c+N), правый край стоит на месте. Нажатия мышью игра
//! проверяет в отдельном коде (52.05: 0x1a4cd0), который считает тот же
//! прямоугольник своей копией формулы — «правый край − 0x25, левый край
//! = правый − 0x3c» и «низ = высота экрана − 4». Хук меняет ширину и там,
//! поэтому вкладки, значки и переключатели остаются кликабельными там,
//! где нарисованы. Правый край не двигается — переключатель существ в
//! клетке и всё, что справа от окна, остаётся на месте.
//!
//! В 0.28.x окно росло вправо, а значки у правого края возвращались на
//! старое место по одному — каждая такая кнопка требовала отдельной
//! правки. Теперь раскладка для рисования и для нажатий меняется вместе.
//! Не нашлась копия формулы для нажатий — окно не расширяется вообще.
//!
//! N берётся из измерения: границы клеток (tiles.rs, detect_border)
//! показывают, на сколько русский текст вылезает за край окна или за
//! линию между колонками. Измерения собираются полсекунды с первого
//! вылета, после чего окно расширяется ОДИН раз: N = 2 × наибольший вылет
//! + 2 клетки запаса, не больше MAX_EXTRA. Дальше ширина не меняется до
//! перезапуска игры: в 0.28.0 окно расширялось по замерам, сделанным во
//! время перестройки после прошлого расширения, и раздувалось до 101 клетки.
//!
//! Место формулы находится в коде по её байтам, как кнопки. Не нашлось
//! (другая версия игры) — окно не меняется. Аварийный выключатель —
//! строка windows_off в dfint-data/ai/experiments.txt.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Предел расширения, клеток: правый край не может уйти дальше 0x18
/// клеток (до края, где у игры начинается своя полоса).
const MAX_EXTRA: usize = 20;
/// Сколько собирать замеры с первого вылета, мс.
const COLLECT_MS: usize = 500;

/// Адрес числа −0x3c в копии формулы для нажатий (lea r, [r − 0x3c]).
static SITE_INPUT: AtomicUsize = AtomicUsize::new(0);
/// Адрес числа 0x3c (ширина) и 0x18 (отступ правого края); 0 — не найдено.
static SITE: AtomicUsize = AtomicUsize::new(0);
static SITE_RIGHT: AtomicUsize = AtomicUsize::new(0);
/// Исходные числа.
static ORIG: AtomicUsize = AtomicUsize::new(0);
static ORIG_RIGHT: AtomicUsize = AtomicUsize::new(0);
/// Ряды значков у правого края окна (перо, камера…): адрес числа k в
/// «правый край − k + 3·i» и исходное k. Нажатия по ним игра проверяет в
/// другом коде, по старому правому краю, поэтому при росте окна вправо
/// значки остаются на старом месте: k увеличивается на ту же прибавку.
static ICONS: std::sync::Mutex<Vec<(usize, u8)>> = std::sync::Mutex::new(Vec::new());
/// Границы функции, которая рисует окно: вылеты из её рамок — к этому окну.
static FUNC_BEGIN: AtomicUsize = AtomicUsize::new(0);
static FUNC_END: AtomicUsize = AtomicUsize::new(0);
/// Наибольший вылет за время сбора, начало сбора (мс от запуска, 0 — не
/// начат) и применённая прибавка (usize::MAX — ещё нет).
static MAX_OVERFLOW: AtomicUsize = AtomicUsize::new(0);
static COLLECT_FROM: AtomicUsize = AtomicUsize::new(0);
static APPLIED: AtomicUsize = AtomicUsize::new(usize::MAX);

fn now_ms() -> usize {
  static START: std::sync::LazyLock<std::time::Instant> = std::sync::LazyLock::new(std::time::Instant::now);
  START.elapsed().as_millis().max(1) as usize
}

#[cfg(target_os = "windows")]
mod os {
  #[link(name = "kernel32")]
  extern "system" {
    fn VirtualProtect(address: *mut core::ffi::c_void, size: usize, protect: u32, old: *mut u32) -> i32;
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn FlushInstructionCache(process: *mut core::ffi::c_void, address: *const core::ffi::c_void, size: usize) -> i32;
  }

  pub unsafe fn write_byte(address: usize, value: u8) -> bool {
    let mut old = 0u32;
    if VirtualProtect(address as *mut _, 1, 0x40, &mut old) == 0 {
      return false;
    }
    std::ptr::write_volatile(address as *mut u8, value);
    let mut ignored = 0u32;
    VirtualProtect(address as *mut _, 1, old, &mut ignored);
    FlushInstructionCache(GetCurrentProcess(), address as *const _, 1);
    true
  }
}

/// Разделы образа игры: (.text, таблица функций .pdata).
#[cfg(target_os = "windows")]
unsafe fn image(base: usize) -> Option<((usize, usize), (usize, usize))> {
  let rd16 = |o: usize| std::ptr::read_unaligned((base + o) as *const u16) as usize;
  let rd32 = |o: usize| std::ptr::read_unaligned((base + o) as *const u32) as usize;
  if rd16(0) != 0x5a4d {
    return None;
  }
  let nt = rd32(0x3c);
  if rd32(nt) != 0x4550 {
    return None;
  }
  let optional = nt + 24;
  // Каталог 3 — таблица исключений (.pdata) в PE32+
  let pdata = (rd32(optional + 112 + 3 * 8), rd32(optional + 112 + 3 * 8 + 4));
  let sections = rd16(nt + 6);
  let table = optional + rd16(nt + 20);
  for i in 0..sections {
    let s = table + i * 40;
    let name = std::slice::from_raw_parts((base + s) as *const u8, 8);
    if name.starts_with(b".text") {
      return Some(((rd32(s + 12), rd32(s + 8)), pdata));
    }
  }
  None
}

/// Функция, в которой лежит адрес (по таблице .pdata).
#[cfg(target_os = "windows")]
unsafe fn function_of(base: usize, pdata: (usize, usize), rva: usize) -> Option<(usize, usize)> {
  let count = pdata.1 / 12;
  let entry = |i: usize| {
    let p = base + pdata.0 + i * 12;
    (std::ptr::read_unaligned(p as *const u32) as usize, std::ptr::read_unaligned((p + 4) as *const u32) as usize)
  };
  let (mut lo, mut hi) = (0usize, count);
  while lo < hi {
    let mid = (lo + hi) / 2;
    let (b, e) = entry(mid);
    if rva < b {
      hi = mid;
    } else if rva >= e {
      lo = mid + 1;
    } else {
      return Some((b, e));
    }
  }
  None
}

/// Найти формулу ширины окна дварфа. Вызывается при запуске.
pub unsafe fn attach() {
  if !crate::ai::EXPERIMENTS.english || crate::ai::EXPERIMENTS.fit_off || crate::ai::EXPERIMENTS.windows_off {
    return;
  }
  #[cfg(target_os = "windows")]
  {
    let base = *crate::utils::MODULE;
    let Some(((rva, size), pdata)) = image(base) else { return };
    let code = std::slice::from_raw_parts((base + rva) as *const u8, size);
    // mov eax,[rsp+X] / sub eax,0xd / sub eax,0x18 / mov [rsp+Y],eax /
    // mov eax,[rsp+Y] / sub eax,N / mov [rsp+Z],eax
    let re = regex::bytes::Regex::new(
      r"(?s-u)\x8B\x84\x24....\x83\xE8\x0D\x83\xE8(.)\x89\x84\x24(....)\x8B\x84\x24....\x83\xE8(.)\x89\x84\x24",
    )
    .unwrap();
    let found: Vec<_> = re.captures_iter(code).collect();
    if found.len() != 1 {
      log::info!("windows: unit sheet width formula found {} times, window is left as is", found.len());
      return;
    }
    let right = found[0].get(1).unwrap();
    let slot = found[0].get(2).unwrap().as_bytes().to_vec();
    let m = found[0].get(3).unwrap();
    let n = m.as_bytes()[0] as usize;
    let r = right.as_bytes()[0] as usize;
    if !(20..=100).contains(&n) || !(4..=60).contains(&r) {
      return;
    }
    let site_rva = rva + m.start();
    ORIG_RIGHT.store(r, Ordering::Relaxed);
    SITE_RIGHT.store(base + rva + right.start(), Ordering::Relaxed);
    if let Some((b, e)) = function_of(base, pdata, site_rva) {
      FUNC_BEGIN.store(b, Ordering::Relaxed);
      FUNC_END.store(e, Ordering::Relaxed);
      // Ряды значков: mov eax,[rsp+правый край] / (dec eax) / sub eax,k /
      // imul ecx,[rsp+i],3 / add eax,ecx — в той же функции
      let mut pattern = String::from(r"(?s-u)\x8B\x84\x24");
      for byte in &slot {
        pattern.push_str(&format!(r"\x{:02X}", byte));
      }
      pattern.push_str(r"(?:\xFF\xC8)?\x83\xE8(.)\x6B\x8C\x24....\x03\x03\xC1");
      // Перо, камера и стрелка «перейти» в правом верхнем углу: их место
      // отсчитывается от «правый край − 3» (52.05: 0x1573c9, по записи
      // icons: перо — этот отступ − 6, камера и стрелка — − 2)
      let mut corner = String::from(r"(?s-u)\x8B\x84\x24");
      for byte in &slot {
        corner.push_str(&format!(r"\x{:02X}", byte));
      }
      corner.push_str(r"\x83\xE8(\x03)\x89\x84\x24");
      if let Ok(corner_re) = regex::bytes::Regex::new(&corner) {
        let func = &code[b - rva..(e - rva).min(code.len())];
        let found: Vec<_> = corner_re.captures_iter(func).collect();
        if found.len() == 1 {
          let k = found[0].get(1).unwrap();
          ICONS.lock().unwrap().push((base + b + k.start(), k.as_bytes()[0]));
          log::info!("windows: unit sheet corner icons at 0x{:x}, offset {}", b + k.start(), k.as_bytes()[0]);
        } else {
          log::info!("windows: unit sheet corner icons found {} times, left as is", found.len());
        }
      }
      if let Ok(icons_re) = regex::bytes::Regex::new(&pattern) {
        let func = &code[b - rva..(e - rva).min(code.len())];
        let mut icons = ICONS.lock().unwrap();
        for c in icons_re.captures_iter(func) {
          let k = c.get(1).unwrap();
          let at = base + b + k.start();
          icons.push((at, k.as_bytes()[0]));
          log::info!("windows: unit sheet icon row at 0x{:x}, offset {}", b + k.start(), k.as_bytes()[0]);
        }
      }
    }
    ORIG.store(n, Ordering::Relaxed);
    SITE.store(base + site_rva, Ordering::Relaxed);
    log::info!("windows: unit sheet width at 0x{site_rva:x}, width {}", n + 1);

    // Копия формулы для нажатий: add r, −0x25 / lea r, [r − 0x3c], и рядом
    // низ окна «высота экрана − 4» (add r, −4). Ровно одна — иначе окно
    // не расширяется
    let input_re = regex::bytes::Regex::new(
      r"(?s-u)(?:\x41)?\x83[\xC0-\xC7]\xDB(?:\x41|\x44|\x45)?\x8D(?:[\x40-\x7F]\x24|[\x40-\x7F])(\xC4)",
    )
    .unwrap();
    let low4 = regex::bytes::Regex::new(r"(?s-u)\x83[\xC0-\xC7]\xFC").unwrap();
    let inputs: Vec<usize> = input_re
      .captures_iter(code)
      .filter(|c| {
        let end = c.get(0).unwrap().end();
        low4.is_match(&code[end..(end + 30).min(code.len())])
      })
      .map(|c| c.get(1).unwrap().start())
      .collect();
    if inputs.len() != 1 {
      log::info!("windows: unit sheet input layout found {} times, window is left as is", inputs.len());
      SITE.store(0, Ordering::Relaxed);
      return;
    }
    SITE_INPUT.store(base + rva + inputs[0], Ordering::Relaxed);
    log::info!("windows: unit sheet input layout at 0x{:x}", rva + inputs[0]);
  }
}

/// Адрес из функции, которая рисует окно дварфа.
pub fn in_window_code(rva: usize) -> bool {
  let (b, e) = (FUNC_BEGIN.load(Ordering::Relaxed), FUNC_END.load(Ordering::Relaxed));
  b != 0 && rva >= b && rva < e
}

/// Вылет текста за край окна или за линию колонок (tiles.rs): from — откуда
/// в коде игры нарисована граница, overflow — на сколько клеток не хватило.
pub fn overflow(from_rva: usize, overflow: usize) {
  if SITE.load(Ordering::Relaxed) == 0 || APPLIED.load(Ordering::Relaxed) != usize::MAX {
    return;
  }
  let (b, e) = (FUNC_BEGIN.load(Ordering::Relaxed), FUNC_END.load(Ordering::Relaxed));
  if b == 0 || from_rva < b || from_rva >= e {
    return;
  }
  let _ = COLLECT_FROM.compare_exchange(0, now_ms(), Ordering::Relaxed, Ordering::Relaxed);
  MAX_OVERFLOW.fetch_max(overflow, Ordering::Relaxed);
}

/// Применить прибавку — один раз, когда замеры собраны. Основной поток,
/// перед выводом надписи (ai::screen).
pub fn refresh() {
  let site = SITE.load(Ordering::Relaxed);
  if site == 0 || APPLIED.load(Ordering::Relaxed) != usize::MAX {
    return;
  }
  let from = COLLECT_FROM.load(Ordering::Relaxed);
  if from == 0 || now_ms() < from + COLLECT_MS {
    return;
  }
  let orig = ORIG.load(Ordering::Relaxed);
  let input = SITE_INPUT.load(Ordering::Relaxed);
  // Каждая колонка получает половину прибавки; две клетки — запас
  let extra = (2 * MAX_OVERFLOW.load(Ordering::Relaxed) + 2).min(MAX_EXTRA);
  APPLIED.store(extra, Ordering::Relaxed);
  if extra == 0 || input == 0 {
    return;
  }
  #[cfg(target_os = "windows")]
  unsafe {
    // Сначала нажатия, потом рисование: на один кадр щелчок может уйти
    // мимо, но окно не окажется нарисованным там, где его не ждут дольше
    let ok = os::write_byte(input, (-((orig + extra) as i32)) as i8 as u8) && os::write_byte(site, (orig + extra) as u8);
    if ok {
      log::info!("windows: unit sheet width {} -> {}, grows to the left", orig + 1, orig + extra + 1);
    }
  }
}
