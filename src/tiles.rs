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
//! Функции картинок — маленькие листовые функции, и компилятор игры
//! знает, какие регистры они не трогают: вызывающий код держит в r11 и
//! других «временных» регистрах указатели через вызов (цикл по значкам в
//! 0xf9d40). Обычный перехват на Rust эти регистры портил, и игра падала
//! в главном меню. Поэтому перехват идёт через переходник на ассемблере:
//! он сохраняет все временные регистры (rax, rcx, rdx, r8–r11, xmm0–xmm5),
//! вызывает проверку и прыгает в исходную функцию с нетронутыми
//! регистрами — для игры вызов выглядит так же, как без хука.
//!
//! Перехват дешёвый: картинка проверяется, только если на её ряду в этом
//! кадре была надпись, которой может не хватить места (русская длиннее
//! английской). Таких рядов немного, остальные картинки проходят без
//! блокировок.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use std::sync::atomic::AtomicUsize;
use std::sync::OnceLock;

use retour::RawDetour;

use crate::ai;
use crate::utils;

/// Ряды (0..255), где в этом кадре была надпись длиннее английской.
static ROWS: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Отладка: сколько строк уже записано в лог (не больше 40 каждого вида).
static LOG_WATCH: AtomicUsize = AtomicUsize::new(0);
static LOG_HIT: AtomicUsize = AtomicUsize::new(0);

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
    let known = st.spans.get(&y).is_some_and(|v| v.iter().any(|s| s.x == x));
    st.spans.entry(y).or_default().push(Span { x, end: x + len as i32, key: key.to_vec() });
    ROWS[i].fetch_or(bit, Ordering::Relaxed);
    if !known && LOG_WATCH.fetch_add(1, Ordering::Relaxed) < 40 {
      log::info!("tiles: watch ({x},{y}) len={len} english={english_len}");
    }
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
    if !st.covered.contains_key(&k) && LOG_HIT.fetch_add(1, Ordering::Relaxed) < 40 {
      log::info!("tiles: picture at ({tx},{ty}) covers text at ({},{}) -> room {room}", k.0, k.1);
    }
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
  // 0xa9a660, 0xa9a7b0, 0xa9ae30: mov r8, rcx; mov r10d, edx; mov ecx, [rcx+0x84]; cmp ecx, [r8+0x288]
  &[0x4c, 0x8b, 0xc1, 0x44, 0x8b, 0xd2, 0x8b, 0x89, 0x84, 0, 0, 0, 0x41, 0x3b, 0x88, 0x88, 0x02, 0, 0],
];

/// Адреса исходных функций (переходник retour) для каждого переходника.
static ORIG_A: AtomicUsize = AtomicUsize::new(0);
static ORIG_B: AtomicUsize = AtomicUsize::new(0);
static ORIG_C: AtomicUsize = AtomicUsize::new(0);
static ORIG_D: AtomicUsize = AtomicUsize::new(0);

/// Проверка из переходника: в rcx — gps, как у исходной функции.
extern "C" fn tile_entry(gps: usize) {
  tile(gps);
}

/// Переходник: сохранить временные регистры, вызвать tile_entry(rcx),
/// восстановить регистры и прыгнуть в исходную функцию.
/// На входе rsp ≡ 8 (mod 16); после 7 push — ≡ 0; 0x80 байт под xmm и
/// теневое пространство сохраняют выравнивание для вызова.
macro_rules! stub {
  ($name:ident, $orig:ident) => {
    #[unsafe(naked)]
    unsafe extern "C" fn $name() {
      core::arch::naked_asm!(
        "push rax",
        "push rcx",
        "push rdx",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "sub rsp, 0x80",
        "movdqu [rsp + 0x20], xmm0",
        "movdqu [rsp + 0x30], xmm1",
        "movdqu [rsp + 0x40], xmm2",
        "movdqu [rsp + 0x50], xmm3",
        "movdqu [rsp + 0x60], xmm4",
        "movdqu [rsp + 0x70], xmm5",
        "call {entry}",
        "movdqu xmm0, [rsp + 0x20]",
        "movdqu xmm1, [rsp + 0x30]",
        "movdqu xmm2, [rsp + 0x40]",
        "movdqu xmm3, [rsp + 0x50]",
        "movdqu xmm4, [rsp + 0x60]",
        "movdqu xmm5, [rsp + 0x70]",
        "add rsp, 0x80",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdx",
        "pop rcx",
        "pop rax",
        "jmp qword ptr [rip + {orig}]",
        entry = sym tile_entry,
        orig = sym $orig,
      )
    }
  };
}

stub!(stub_a, ORIG_A);
stub!(stub_b, ORIG_B);
stub!(stub_c, ORIG_C);
stub!(stub_d, ORIG_D);

static DETOURS: OnceLock<Vec<RawDetour>> = OnceLock::new();

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
  // Переходник рассчитан на соглашение о вызовах Windows (аргумент в rcx)
  if cfg!(not(target_os = "windows")) || !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off {
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
  let stubs: [unsafe extern "C" fn(); 4] = [stub_a, stub_b, stub_c, stub_d];
  let origs = [&ORIG_A, &ORIG_B, &ORIG_C, &ORIG_D];
  let mut detours = Vec::new();
  for (n, &addr) in found.iter().enumerate() {
    let made = unsafe { RawDetour::new(addr as *const (), stubs[n] as *const ()) };
    match made {
      Ok(detour) => {
        // Адрес исходной функции — до включения перехвата
        origs[n].store(detour.trampoline() as *const () as usize, Ordering::SeqCst);
        match unsafe { detour.enable() } {
          Ok(_) => detours.push(detour),
          Err(err) => log::warn!("tiles: unable to enable 0x{:x}: {err:?}", addr - utils::address(0)),
        }
      }
      Err(err) => log::warn!("tiles: unable to attach 0x{:x}: {err:?}", addr - utils::address(0)),
    }
  }
  log::info!("tiles: {} drawing functions attached", detours.len());
  let _ = DETOURS.set(detours);
}
