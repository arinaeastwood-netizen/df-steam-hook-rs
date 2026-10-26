//! Картинки поверх текста.
//!
//! В таблицах игра рисует надпись, а следом, на фиксированном месте
//! справа, — картинки (значки навыков на странице «Труд»). Колонка
//! рассчитана под английский текст, и длинная русская надпись уходит под
//! значок: «Профессиональный шахт». Ни ограничения длины, ни рамки игра
//! при этом не задаёт — место ограничивает только картинка.
//!
//! Хук перехватывает функции, которыми игра рисует картинку в клетку
//! (52.05: 0xa9ac90, 0xa9a660, 0xa9a7b0, 0xa9ae30 — по одной на каждый
//! слой картинок; ищутся по началу кода, так что
//! после обновления игры находятся сами). Картинка, нарисованная ПОСЛЕ
//! надписи в том же кадре и внутри её клеток, — это то, что закрывает
//! надпись. Фон под надписью игра рисует раньше текста, поэтому он не
//! считается. Для такой надписи запоминается, сколько клеток до картинки;
//! fit.rs сокращает надпись до этого места.
//!
//! Перехват дешёвый: картинка проверяется, только если на её ряду в этом
//! кадре была надпись, которой может не хватить места (русская длиннее
//! английской). Таких рядов немного, остальные картинки проходят без
//! блокировок.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use retour::static_detour;

use crate::ai;
use crate::utils;

static_detour! {
  static TILE_A: unsafe extern "system" fn(usize, u32, u8);
  static TILE_B: unsafe extern "system" fn(usize, u32, u8);
  static TILE_C: unsafe extern "system" fn(usize, u32, u8);
  static TILE_D: unsafe extern "system" fn(usize, u32, u8);
}

/// Ряды (0..255), где в этом кадре была надпись длиннее английской.
static ROWS: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

fn row_bit(y: i32) -> Option<(usize, u64)> {
  if !(0..256).contains(&y) {
    return None;
  }
  Some(((y / 64) as usize, 1u64 << (y % 64)))
}

struct Span {
  x: i32,
  end: i32,
  key: Vec<u8>,
}

struct State {
  /// Надписи этого кадра, которым может не хватить места: ряд → надписи
  spans: HashMap<i32, Vec<Span>>,
  /// Надпись (столбец, ряд, английский ключ) → клеток до картинки
  covered: HashMap<(i32, i32, Vec<u8>), usize>,
}

static STATE: LazyLock<Mutex<State>> =
  LazyLock::new(|| Mutex::new(State { spans: HashMap::new(), covered: HashMap::new() }));

/// Новый кадр: надписи прошлого кадра забываются (fit.rs, смена кадра).
pub fn next_frame() {
  for r in &ROWS {
    r.store(0, Ordering::Relaxed);
  }
  if let Ok(mut st) = STATE.try_lock() {
    st.spans.clear();
  }
}

/// Надпись выведена: если она длиннее английской, её клетки
/// проверяются на картинки, нарисованные после неё.
pub fn text(x: i32, y: i32, len: usize, english_len: usize, key: &[u8]) {
  if len <= english_len {
    return;
  }
  let Some((i, bit)) = row_bit(y) else { return };
  if let Ok(mut st) = STATE.try_lock() {
    st.spans.entry(y).or_default().push(Span { x, end: x + len as i32, key: key.to_vec() });
    ROWS[i].fetch_or(bit, Ordering::Relaxed);
  }
}

/// Сколько клеток у надписи до картинки, которая её закрывает (если
/// такая картинка была замечена).
pub fn room(x: i32, y: i32, key: &[u8]) -> Option<usize> {
  STATE.try_lock().ok()?.covered.get(&(x, y, key.to_vec())).copied()
}

fn tile(gps: usize) {
  if gps == 0 || !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off {
    return;
  }
  let (tx, ty) = ai::cursor(gps);
  let Some((i, bit)) = row_bit(ty) else { return };
  if ROWS[i].load(Ordering::Relaxed) & bit == 0 {
    return;
  }
  let Ok(mut st) = STATE.try_lock() else { return };
  let mut hits = Vec::new();
  if let Some(spans) = st.spans.get(&ty) {
    for s in spans {
      if tx > s.x && tx < s.end {
        hits.push(((s.x, ty, s.key.clone()), (tx - s.x - 1) as usize));
      }
    }
  }
  if st.covered.len() > 4096 {
    st.covered.clear();
  }
  for (k, room) in hits {
    let e = st.covered.entry(k).or_insert(room);
    if room < *e {
      *e = room;
    }
  }
}

// ==================== ПОИСК И ПЕРЕХВАТ ====================

/// Начала функций, рисующих картинку в клетку курсора (52.05). Общая
/// часть: проверка курсора по рамке отсечения gps[0x288..0x294].
const PATTERNS: &[&[u8]] = &[
  // 0xa9ac90: mov r9, rcx; mov r10d, edx; mov ecx, [rcx+0x84]; cmp ecx, [r9+0x288]
  &[0x4c, 0x8b, 0xc9, 0x44, 0x8b, 0xd2, 0x8b, 0x89, 0x84, 0, 0, 0, 0x41, 0x3b, 0x89, 0x88, 0x02, 0, 0],
  // 0xa9a660, 0xa9a7b0: mov r8, rcx; mov r10d, edx; mov ecx, [rcx+0x84]; cmp ecx, [r8+0x288]
  &[0x4c, 0x8b, 0xc1, 0x44, 0x8b, 0xd2, 0x8b, 0x89, 0x84, 0, 0, 0, 0x41, 0x3b, 0x88, 0x88, 0x02, 0, 0],
];

/// Секция кода игры в памяти.
fn text_section() -> Option<&'static [u8]> {
  unsafe {
    let base = utils::address(0);
    let pe = base + std::ptr::read_unaligned((base + 0x3c) as *const u32) as usize;
    if std::ptr::read_unaligned(pe as *const u32) != 0x4550 {
      return None;
    }
    let count = std::ptr::read_unaligned((pe + 6) as *const u16) as usize;
    let opt = std::ptr::read_unaligned((pe + 20) as *const u16) as usize;
    let table = pe + 24 + opt;
    for i in 0..count {
      let s = table + i * 40;
      let name = std::slice::from_raw_parts(s as *const u8, 8);
      if name.starts_with(b".text") {
        let vsize = std::ptr::read_unaligned((s + 8) as *const u32) as usize;
        let va = std::ptr::read_unaligned((s + 12) as *const u32) as usize;
        return Some(std::slice::from_raw_parts((base + va) as *const u8, vsize));
      }
    }
    None
  }
}

/// Перехват картинок. Вызывается при запуске, до того как игра начнёт
/// рисовать. Не нашлось — работает всё остальное, только без картинок.
pub fn attach() {
  if !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off {
    return;
  }
  let Some(code) = text_section() else {
    log::warn!("tiles: code section not found");
    return;
  };
  let mut found: Vec<usize> = Vec::new();
  for pattern in PATTERNS {
    let mut at = 0;
    while let Some(pos) = code[at..].windows(pattern.len()).position(|w| w == *pattern) {
      found.push(code.as_ptr() as usize + at + pos);
      at += pos + 1;
    }
  }
  if found.is_empty() || found.len() > 4 {
    log::warn!("tiles: {} drawing functions found, expected 1..4 — not attached", found.len());
    return;
  }
  let detours = [&TILE_A, &TILE_B, &TILE_C, &TILE_D];
  let mut attached = 0;
  for (n, &addr) in found.iter().enumerate() {
    let target: unsafe extern "system" fn(usize, u32, u8) = unsafe { std::mem::transmute(addr) };
    let detour = detours[n];
    let ok = unsafe {
      match n {
        0 => detour.initialize(target, |gps, tex, flag| {
          tile(gps);
          TILE_A.call(gps, tex, flag)
        }),
        1 => detour.initialize(target, |gps, tex, flag| {
          tile(gps);
          TILE_B.call(gps, tex, flag)
        }),
        2 => detour.initialize(target, |gps, tex, flag| {
          tile(gps);
          TILE_C.call(gps, tex, flag)
        }),
        _ => detour.initialize(target, |gps, tex, flag| {
          tile(gps);
          TILE_D.call(gps, tex, flag)
        }),
      }
      .and_then(|d| d.enable())
    };
    match ok {
      Ok(_) => attached += 1,
      Err(err) => log::warn!("tiles: unable to attach 0x{:x}: {err:?}", addr - utils::address(0)),
    }
  }
  log::info!("tiles: {attached} drawing functions attached");
}
