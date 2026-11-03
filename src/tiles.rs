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
/// Сколько найденных границ уже записано в лог.
static LOG_PROBE: AtomicUsize = AtomicUsize::new(0);
/// Ряды с длинными надписями в прошлом кадре: картинки на них
/// запоминаются весь кадр — и до надписи, и после (detect_border).
static PREV_ROWS: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
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
  /// Длина английского оригинала: картинка ближе этого — не значок
  /// поверх надписи, а фон или чужая разметка, её не считаем
  english: usize,
  /// В этом проходе уже была картинка там, где стоял бы английский текст:
  /// это фон следующего кадра (игра рисует фон перед текстом, слева
  /// направо). Всё, что нарисовано после него, к этой надписи не относится
  background: bool,
  /// Ближайшая картинка поверх надписи в этом проходе (до фона)
  candidate: Option<usize>,
  /// Когда надпись выведена: картинка считается закрывающей её, только
  /// если нарисована сразу после — в том же проходе отрисовки. Фон
  /// следующего кадра игра рисует раньше текста, но уже через десятки
  /// миллисекунд, и по времени он отсекается. Граница кадра по первой
  /// надписи для этого не годится: фон начала кадра попадает в прошлый.
  at: std::time::Instant,
}

/// Граница прохода — фон (см. Span::background); время только страхует
/// от совсем старых надписей.
const OVERLAY_WINDOW: std::time::Duration = std::time::Duration::from_millis(40);

struct State {
  /// Надписи этого кадра, которым может не хватить места: ряд → надписи
  spans: HashMap<i32, Vec<Span>>,
  /// Картинки за кадр на рядах с длинными надписями — фон, сетка, рамка
  /// окна: ряд → (столбец, номер картинки, функция A–D)
  frame_tiles: HashMap<i32, Vec<(i32, u32, u8)>>,
  /// То же, собранное перед текущим кадром
  last_tiles: HashMap<i32, Vec<(i32, u32, u8)>>,
  /// Надпись (столбец, ряд, английский ключ) → клеток до границы клетки
  bordered: HashMap<(i32, i32, Vec<u8>), usize>,
  /// Надпись (столбец, ряд, английский ключ) → клеток до картинки
  covered: HashMap<(i32, i32, Vec<u8>), usize>,
  /// Отладка: надписи, уже записанные в лог
  logged: Vec<(i32, i32)>,
}

static STATE: LazyLock<Mutex<State>> =
  LazyLock::new(|| {
    Mutex::new(State {
      spans: HashMap::new(),
      frame_tiles: HashMap::new(),
      last_tiles: HashMap::new(),
      bordered: HashMap::new(),
      covered: HashMap::new(),
      logged: Vec::new(),
    })
  });

/// Сколько найденных границ записать в лог.
const PROBE_LIMIT: usize = 60;
/// Предел картинок, запоминаемых на ряд за кадр.
const PROBE_ROW_TILES: usize = 1024;

/// Новый кадр: надписи прошлого кадра забываются (fit.rs, смена кадра).
pub fn next_frame() {
  for (r, prev) in ROWS.iter().zip(PREV_ROWS.iter()) {
    prev.store(r.swap(0, Ordering::Relaxed), Ordering::Relaxed);
  }
  if let Ok(mut st) = STATE.try_lock() {
    st.spans.clear();
    st.last_tiles = std::mem::take(&mut st.frame_tiles);
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
    let at = std::time::Instant::now();
    // Та же надпись на новом кадре: картинка, найденная в прошлом проходе
    // поверх неё (до фона следующего кадра), запоминается
    let mut done: Option<(Vec<u8>, usize)> = None;
    if let Some(row) = st.spans.get(&y) {
      if let Some(old) = row.iter().find(|s| s.x == x) {
        if let Some(room) = old.candidate {
          done = Some((old.key.clone(), room));
        }
      }
    }
    if let Some((old_key, room)) = done {
      if !st.covered.contains_key(&(x, y, old_key.clone())) && LOG_HIT.fetch_add(1, Ordering::Relaxed) < 40 {
        log::info!("tiles: picture covers text at ({x},{y}) -> room {room}");
      }
      let e = st.covered.entry((x, y, old_key)).or_insert(room);
      if room < *e {
        *e = room;
      }
    }
    let row = st.spans.entry(y).or_default();
    row.retain(|s| s.x != x);
    row.push(Span {
      x,
      end: x + len as i32,
      key: key.to_vec(),
      english: english_len,
      background: false,
      candidate: None,
      at,
    });
    ROWS[i].fetch_or(bit, Ordering::Relaxed);
    if st.bordered.len() > 4096 {
      st.bordered.clear();
    }
    detect_border(&mut st, x, y, len, english_len, key);
    if !known && !st.logged.contains(&(x, y)) && LOG_WATCH.fetch_add(1, Ordering::Relaxed) < 60 {
      st.logged.push((x, y));
      log::info!("tiles: watch ({x},{y}) len={len} english={english_len}");
    }
  }
}

/// Граница клетки по картинкам, которые игра нарисовала на ряду надписи
/// за прошлый кадр (рамки, сетку и фон игра рисует до текста, поэтому
/// «картинка поверх надписи» их не ловит).
///
/// По записи с экрана (52.05, окно дварфа): под надписью лежит подложка
/// окна (B104335), правый край окна — отдельная картинка в одну клетку
/// (B104336), линия между колонками — картинка поверх подложки (A104812);
/// за окном — пустые клетки. Отсюда правило без привязки к номерам:
///
///   подложка — картинки под первыми клетками надписи;
///   граница — первая клетка правее начала надписи, где есть картинка не
///   из подложки, а в соседних клетках слева и справа таких картинок нет
///   (одиночная вертикальная линия). Горизонтальные полосы и значки из
///   нескольких клеток сюда не попадают: у них соседи тоже не из подложки.
///
/// Место надписи — клеток до границы.
fn detect_border(st: &mut State, x: i32, y: i32, len: usize, english: usize, key: &[u8]) -> Option<usize> {
  let tiles = st.last_tiles.get(&y)?;
  let end = x + len as i32;
  let mut columns: HashMap<i32, Vec<(u32, u8)>> = HashMap::new();
  // Картинка 0 — пустая клетка, она стоит везде и ни о чём не говорит
  for t in tiles.iter().filter(|t| t.1 != 0 && t.0 >= x - 1 && t.0 <= end + 1) {
    let cell = columns.entry(t.0).or_default();
    if !cell.contains(&(t.1, t.2)) {
      cell.push((t.1, t.2));
    }
  }
  let base: std::collections::HashSet<(u32, u8)> = (x..(x + 2).min(end))
    .filter_map(|c| columns.get(&c))
    .flatten()
    .copied()
    .collect();
  if base.is_empty() {
    return None;
  }
  let foreign = |c: i32| -> bool { columns.get(&c).is_some_and(|v| v.iter().any(|t| !base.contains(t))) };
  let border = (x + 2..end).find(|&c| {
    if !foreign(c) || foreign(c - 1) || foreign(c + 1) {
      return false;
    }
    let room = (c - x) as usize;
    if room >= english {
      return true;
    }
    // Место меньше английской длины — надпись сдвинута вправо русским
    // словом перед ней. Тогда граница — только настоящий край: за ней
    // подложка окна кончается (правый край окна), или линию рисует другая
    // функция, поверх подложки (линия между колонками). Полоса-подсветка
    // под надписью тоже кончается картинкой-краем, но подложка окна за ней
    // продолжается — это не край
    let tile_func = columns.get(&c).and_then(|v| v.iter().find(|t| !base.contains(t))).map(|t| t.1);
    let other_func = tile_func.is_some_and(|f| base.iter().all(|b| b.1 != f));
    let after = columns.get(&(c + 1));
    let base_ends = after.map_or(true, |v| !v.iter().any(|t| base.contains(t)));
    other_func || base_ends
  })?;
  let room = (border - x) as usize;

  let first = !st.bordered.contains_key(&(x, y, key.to_vec()));
  st.bordered.insert((x, y, key.to_vec()), room);
  if first && LOG_PROBE.fetch_add(1, Ordering::Relaxed) < PROBE_LIMIT {
    let tile = columns.get(&border).and_then(|v| v.iter().find(|t| !base.contains(t)).copied());
    log::info!(
      "frame: border at ({x},{y}) len={len} room={room} tile={}",
      tile.map_or("-".to_string(), |(n, f)| format!("{}{}", f as char, n))
    );
  }
  Some(room)
}

/// Место надписи до границы клетки (detect_border), если граница найдена.
pub fn border(x: i32, y: i32, key: &[u8]) -> Option<usize> {
  STATE.try_lock().ok()?.bordered.get(&(x, y, key.to_vec())).copied()
}

/// Сколько клеток у надписи до картинки, которая её закрывает (если
/// такая картинка была замечена).
pub fn room(x: i32, y: i32, key: &[u8]) -> Option<usize> {
  STATE.try_lock().ok()?.covered.get(&(x, y, key.to_vec())).copied()
}

thread_local! {
  /// Сейчас выводится текст: буквы шрифта игра рисует теми же функциями
  /// картинок, и они не должны считаться картинками поверх надписи.
  static IN_TEXT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Вывод текста (addst, addst_top, addst_flag): картинки, нарисованные
/// внутри, — это буквы самой надписи, а не значки поверх неё.
pub fn own_text<R>(draw: impl FnOnce() -> R) -> R {
  let was = IN_TEXT.with(|f| f.replace(true));
  let out = draw();
  IN_TEXT.with(|f| f.set(was));
  out
}

fn tile(gps: usize, texpos: u32, func: u8) {
  if gps == 0 || !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off {
    return;
  }
  // Буквы выводимой надписи: шрифт игра рисует картинками
  if IN_TEXT.with(|f| f.get()) {
    return;
  }
  let (tx, ty) = ai::cursor(gps);
  let Some((i, bit)) = row_bit(ty) else { return };
  // Всё, что нарисовано за кадр на рядах с длинными надписями (detect_border)
  if (PREV_ROWS[i].load(Ordering::Relaxed) | ROWS[i].load(Ordering::Relaxed)) & bit != 0 {
    if let Ok(mut st) = STATE.try_lock() {
      let row = st.frame_tiles.entry(ty).or_default();
      if row.len() < PROBE_ROW_TILES {
        row.push((tx, texpos, func));
      }
    }
  }
  if ROWS[i].load(Ordering::Relaxed) & bit == 0 {
    return;
  }
  let Ok(mut st) = STATE.try_lock() else { return };
  let now = std::time::Instant::now();
  if st.covered.len() > 4096 {
    st.covered.clear();
  }
  if let Some(spans) = st.spans.get_mut(&ty) {
    for s in spans.iter_mut() {
      if s.background || now.duration_since(s.at) > OVERLAY_WINDOW {
        continue;
      }
      if tx > s.x && tx <= s.x + s.english as i32 {
        // Картинка там, где стоял английский текст: игра своих значков
        // поверх своего текста не ставит — это фон следующего кадра.
        // Дальше в этом проходе ничего не считается
        s.background = true;
      } else if tx > s.x + s.english as i32 + 1 && tx < s.end {
        // Значок справа от английского текста, внутри русского. Место
        // строго больше английской длины: значок вплотную за английским
        // текстом без пробела не бывает
        let room = (tx - s.x - 1) as usize;
        s.candidate = Some(s.candidate.map_or(room, |c| c.min(room)));
      }
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

/// Проверка из переходника: в rcx — gps, в edx — номер картинки, как у
/// исходной функции. Буква — какая из четырёх функций рисует.
extern "C" fn tile_entry_a(gps: usize, texpos: u32) {
  tile(gps, texpos, b'A');
}
extern "C" fn tile_entry_b(gps: usize, texpos: u32) {
  tile(gps, texpos, b'B');
}
extern "C" fn tile_entry_c(gps: usize, texpos: u32) {
  tile(gps, texpos, b'C');
}
extern "C" fn tile_entry_d(gps: usize, texpos: u32) {
  tile(gps, texpos, b'D');
}

/// Переходник: сохранить временные регистры, вызвать tile_entry(rcx),
/// восстановить регистры и прыгнуть в исходную функцию.
/// На входе rsp ≡ 8 (mod 16); после 7 push — ≡ 0; 0x80 байт под xmm и
/// теневое пространство сохраняют выравнивание для вызова.
macro_rules! stub {
  ($name:ident, $orig:ident, $entry:ident) => {
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
        entry = sym $entry,
        orig = sym $orig,
      )
    }
  };
}

stub!(stub_a, ORIG_A, tile_entry_a);
stub!(stub_b, ORIG_B, tile_entry_b);
stub!(stub_c, ORIG_C, tile_entry_c);
stub!(stub_d, ORIG_D, tile_entry_d);

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
          Ok(_) => {
            log::info!("tiles: function {} at 0x{:x}", (b'A' + n as u8) as char, addr - utils::address(0));
            detours.push(detour)
          }
          Err(err) => log::warn!("tiles: unable to enable 0x{:x}: {err:?}", addr - utils::address(0)),
        }
      }
      Err(err) => log::warn!("tiles: unable to attach 0x{:x}: {err:?}", addr - utils::address(0)),
    }
  }
  log::info!("tiles: {} drawing functions attached", detours.len());
  let _ = DETOURS.set(detours);
}
