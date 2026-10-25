//! Надписи, которым не хватает места (режим оригинального текста).
//!
//! Игра раскладывает экран под английский текст: вкладки и колонки
//! верхней панели стоят на своих местах, сколько бы букв ни было в
//! надписи. Русский перевод длиннее и налезает на соседа справа:
//! «Растения» склеивается с «Прочее», «Предметы» — со «Здоровье».
//!
//! Хук помнит, где начинались надписи на прошлом кадре (экран
//! перерисовывается каждый кадр). Если надпись дотягивается до начала
//! соседней в том же ряду, она обрезается за клетку до соседа, а в журнал
//! уходит запись W: сколько клеток есть у этой надписи. SkadDF переводит
//! её заново с ограничением длины — модель подбирает слово короче
//! («Флора», «Комн.»), и обрезка больше не нужна.
//!
//! Запись W: «ширина» 0x1E текст. Текст — английский оригинал, если
//! перевод подставлен при выводе, или сама русская надпись, если игра
//! собрала её из словаря интерфейса.
//!
//! Кадры различаются по первой надписи: когда она выводится снова в том
//! же месте, начался следующий кадр. Если она не повторяется (экран
//! сменился), новый кадр отсчитывается через FRAME_FALLBACK выводов.
//!
//! Второй источник: рамка отсечения. Окна и колонки игра рисует с
//! рамкой (gps.clipx): буквы правее её края просто не выводятся. Так
//! обрезались «Мир: Алабеатира, "» в списке миров (колонка «Название»
//! шириной под английское имя) и дата в панели погоды. Если английская
//! надпись в рамку помещается, а русская — нет, ширина до края рамки
//! уходит в журнал так же, записью W.
//!
//! Слова фраз летописи и строки абзацев сюда не попадают — у них своя
//! раскладка (flow.rs, ai::screen).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::ai;

const FRAME_FALLBACK: u32 = 30_000;

/// Рамка отсечения в структуре графики 52.05: clipx[0], clipx[1] — так
/// их читает вывод символа (0xc0780: cmp r11d, [r9+0x288] / [r9+0x28c]).
const GPS_CLIPX0: usize = 0x288;
const GPS_CLIPX1: usize = 0x28c;
const GPS_CLIPY0: usize = 0x290;
const GPS_CLIPY1: usize = 0x294;

/// Видна ли надпись, начатая в (x, y): клетка внутри рамки отсечения.
/// Игра рисует и то, чего не видно, — например, список сохранений мира
/// за пределами своей области. Такие надписи соседями не считаются:
/// иначе невидимое «Крепость» на том же ряду обрезало имя мира
/// («Мир: Гоматсил, "Легендарные»).
fn visible(gps: usize, x: i32, y: i32) -> bool {
  let read = |off: usize| unsafe { std::ptr::read_unaligned((gps + off) as *const i32) };
  let (x0, x1, y0, y1) = (read(GPS_CLIPX0), read(GPS_CLIPX1), read(GPS_CLIPY0), read(GPS_CLIPY1));
  if x1 < x0 || y1 < y0 || x1 > 2000 || y1 > 2000 {
    return true;
  }
  x0 <= x && x <= x1 && y0 <= y && y <= y1
}

/// Сколько клеток до правого края рамки отсечения (включая клетку x).
fn clip_room(gps: usize, x: i32) -> Option<usize> {
  let (x0, x1) = unsafe {
    (
      std::ptr::read_unaligned((gps + GPS_CLIPX0) as *const i32),
      std::ptr::read_unaligned((gps + GPS_CLIPX1) as *const i32),
    )
  };
  if x0 < 0 || x1 < x0 || x1 > 2000 || x < x0 || x > x1 {
    return None;
  }
  Some((x1 - x + 1) as usize)
}
/// Сколько кадров подряд надпись должна не помещаться, чтобы уйти в журнал:
/// первое слово новой фразы летописи на первом кадре ещё не узнано как
/// фраза и налезает на следующее слово — это не повод сокращать перевод
const REPORT_AFTER: u32 = 3;
const MAX_ROWS: usize = 400;
const MAX_REPORTS: usize = 4096;

struct Tracker {
  gen: u32,
  calls: u32,
  mark: Option<(i32, i32, usize)>,
  /// Ряд → начала надписей и кадр, на котором их видели
  rows: HashMap<i32, Vec<(i32, u32)>>,
  /// Надпись → сколько кадров не помещалась, на каком кадре последний раз
  misses: HashMap<Vec<u8>, (u32, u32)>,
  /// Колонки таблиц (addst_flag): столбец → кадр, длина самой длинной
  /// английской надписи в колонке, ряды, где она встречалась
  columns: HashMap<i32, Column>,
}

#[derive(Default)]
struct Column {
  gen: u32,
  widest: usize,
  rows: Vec<i32>,
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(|| {
  Mutex::new(Tracker {
    gen: 0,
    calls: 0,
    mark: None,
    rows: HashMap::new(),
    misses: HashMap::new(),
    columns: HashMap::new(),
  })
});

fn blank(text: &[u8]) -> bool {
  text.iter().all(|&b| b == b' ' || b == 0)
}

impl Tracker {
  fn next_frame(&mut self, mark: (i32, i32, usize)) {
    self.gen = self.gen.wrapping_add(1);
    self.calls = 0;
    self.mark = Some(mark);
    if self.rows.len() > MAX_ROWS {
      let gen = self.gen;
      self.rows.retain(|_, v| {
        v.retain(|&(_, g)| gen.wrapping_sub(g) <= 2);
        !v.is_empty()
      });
    }
  }

  fn record(&mut self, x: i32, y: i32, len: usize) {
    let mark = (x, y, len);
    match self.mark {
      Some(m) if m == mark => self.next_frame(mark),
      None => self.next_frame(mark),
      _ => {}
    }
    self.calls += 1;
    if self.calls > FRAME_FALLBACK {
      self.next_frame(mark);
    }
    let gen = self.gen;
    let row = self.rows.entry(y).or_default();
    match row.iter_mut().find(|(rx, _)| *rx == x) {
      Some(entry) => entry.1 = gen,
      None => row.push((x, gen)),
    }
  }

  /// Надпись в колонке таблицы: запомнить её английскую длину. Колонка
  /// живёт два кадра: прошлый (чтобы знать ряды ниже) и текущий.
  fn column_note(&mut self, x: i32, y: i32, english_len: usize) {
    if self.columns.len() > 256 {
      let gen = self.gen;
      self.columns.retain(|_, c| gen.wrapping_sub(c.gen) <= 1);
    }
    let gen = self.gen;
    let col = self.columns.entry(x).or_default();
    if gen.wrapping_sub(col.gen) > 1 {
      *col = Column::default();
    }
    col.gen = gen;
    col.widest = col.widest.max(english_len);
    if !col.rows.contains(&y) {
      col.rows.push(y);
    }
  }

  /// Ширина колонки таблицы по английскому тексту: самая длинная
  /// английская надпись колонки и запас в 3 клетки. Только если в колонке
  /// хотя бы две надписи на разных рядах — одиночная надпись не колонка.
  fn column_width(&self, x: i32) -> Option<usize> {
    let col = self.columns.get(&x)?;
    if self.gen.wrapping_sub(col.gen) > 1 || col.rows.len() < 2 {
      return None;
    }
    Some(col.widest + 3)
  }

  /// Сколько клеток до начала соседней надписи справа (с пробелом перед
  /// ней), если сосед ближе, чем кончается текст длиной len.
  fn room(&self, x: i32, y: i32, len: usize) -> Option<usize> {
    let row = self.rows.get(&y)?;
    let end = x + len as i32;
    row
      .iter()
      .filter(|&&(rx, g)| rx > x && rx < end && self.gen.wrapping_sub(g) <= 1)
      .map(|&(rx, _)| (rx - x - 1).max(0) as usize)
      .min()
  }
}

/// Надпись перед выводом. key — что вывела бы игра, drawn — что выводится
/// на самом деле (перевод без завершающего нуля или None, если перевода
/// нет). flow — слово фразы летописи: у неё своя раскладка.
/// Возвращает, сколько байт надписи выводить, если её нужно обрезать.
/// Сколько строк отладки места уже записано в лог (не больше LOG_LIMIT).
static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const LOG_LIMIT: usize = 60;

/// Запись в лог: надпись длиннее, чем разрешено игрой (space) или рамкой,
/// и что с ней решено. По ней видно, доходит ли до хука ограничение.
fn trace(x: i32, y: i32, text: &[u8], space: u32, clip: Option<usize>, decision: &str) {
  use std::sync::atomic::Ordering;
  if LOGGED.fetch_add(1, Ordering::Relaxed) >= LOG_LIMIT {
    return;
  }
  log::info!(
    "fit: ({x},{y}) len={} space={space} clip={:?} -> {decision}",
    text.len(),
    clip
  );
}

/// column — надпись из addst_flag: так игра рисует колонки таблиц (навык
/// на странице «Труд»). Справа от колонки игра рисует картинки (значки
/// навыков), и они ложатся поверх длинной надписи: «Профессиональный шахт».
/// Картинок хук не видит, поэтому ширину колонки он берёт по английскому
/// тексту той же колонки: самая длинная английская надпись и 3 клетки.
pub fn settle(gps: usize, key: &[u8], drawn: Option<&[u8]>, flow: bool, space: u32, column: bool) -> Option<usize> {
  if gps == 0 || blank(key) {
    return None;
  }
  let (x, y) = ai::cursor(gps);
  // Отладка: надпись длиннее разрешённого — пишется в лог всегда, даже
  // если механизм выключен, чтобы было видно, что приходит от игры
  {
    let text = drawn.unwrap_or(key);
    let clip_now = clip_room(gps, x);
    let over_space = space > 0 && text.len() > space as usize;
    let over_clip = clip_now.is_some_and(|c| text.len() > c);
    if over_space || over_clip {
      let state = if flow {
        "skip: flow word"
      } else if over_space {
        "abbreviate to space"
      } else if !ai::EXPERIMENTS.english {
        "skip: not english mode"
      } else if ai::EXPERIMENTS.fit_off {
        "skip: fit_off"
      } else {
        "abbreviate to clip"
      };
      trace(x, y, text, space, clip_now, state);
    }
    // Ограничение длины от самой игры (space) — настоящее всегда: игра
    // обрежет надпись сама. Сокращение словами лучше обрубка в любом
    // режиме, даже с выключенными «Надписями по месту»
    if over_space && !flow && (!ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off) {
      return Some(space as usize);
    }
  }
  if !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off {
    return None;
  }
  let text = drawn.unwrap_or(key);
  let mut tracker = TRACKER.try_lock().ok()?;
  if visible(gps, x, y) {
    tracker.record(x, y, key.len());
  }
  if flow {
    return None;
  }

  // Место — до соседа справа или до края рамки, что ближе. Край рамки
  // считается, только если английская надпись в неё помещается: иначе
  // так задумано самой игрой
  // Сосед справа считается, только если английская надпись до него
  // помещалась: игра раскладывала экран под английский текст, и
  // настоящий сосед никогда не стоит ближе, чем кончается оригинал. Если
  // ближе — это не сосед, а надпись с другого экрана или слоя (дата из
  // панели крепости под картой мира), и резать по ней нельзя: так
  // «Ледяные Материки» превращались в «Ле». Для надписи, собранной игрой
  // уже русской, английская длина известна только из словаря интерфейса
  let english_len = if drawn.is_some() { Some(key.len()) } else { ai::ui_source_len(key) };
  let neighbor = match english_len {
    Some(len) => tracker.room(x, y, text.len()).filter(|&r| r >= len),
    None => None,
  };
  // Колонка таблицы: место по английскому тексту колонки
  let column_room = if column {
    if let Some(len) = english_len {
      tracker.column_note(x, y, len);
    }
    tracker.column_width(x).filter(|&w| text.len() > w)
  } else {
    None
  };
  if let Some(w) = column_room {
    trace(x, y, text, space, Some(w), "abbreviate to column");
  }
  let neighbor = match (neighbor, column_room) {
    (Some(a), Some(b)) => Some(a.min(b)),
    (a, b) => a.or(b),
  };
  // Край рамки — настоящий край, даже если в него не помещается и
  // английская надпись: колонка «Expert Miner» на странице «Труд» уже
  // английского текста, и игра обрезает его тоже. Русскую надпись тогда
  // нужно сделать короче рамки («Эксп. шахтёр»), а не обрубить на «Экспертный ш»
  let clip = clip_room(gps, x).filter(|&c| text.len() > c);
  // Игра сама сокращает надпись до space знаков — это тоже настоящая граница
  let abbreviated = (space > 0 && text.len() > space as usize).then_some(space as usize);
  let clip = match (clip, abbreviated) {
    (Some(a), Some(b)) => Some(a.min(b)),
    (a, b) => a.or(b),
  };
  let (room, by_clip) = match (neighbor, clip) {
    (Some(a), Some(b)) if b <= a => (b, true),
    (Some(a), _) => (a, false),
    (None, Some(b)) => (b, true),
    (None, None) => return None,
  };
  if room < 2 || room >= text.len() {
    return None;
  }

  // В журнал — английский оригинал, если перевод подставлен здесь, или
  // русская надпись, собранная игрой из словаря интерфейса
  let report = if drawn.is_some() { key } else { text };
  let gen = tracker.gen;
  if tracker.misses.len() > MAX_REPORTS {
    tracker.misses.clear();
  }
  let entry = tracker.misses.entry(report.to_vec()).or_insert((0, gen.wrapping_sub(1)));
  if entry.1 != gen {
    entry.0 += 1;
    entry.1 = gen;
    if entry.0 == REPORT_AFTER {
      // «ширина,c» — по краю рамки (настоящее место, даже если английская
      // надпись длиннее), «ширина,n» — до соседа справа
      let mut payload = format!("{room},{}", if by_clip { 'c' } else { 'n' }).into_bytes();
      payload.push(0x1e);
      payload.extend_from_slice(report);
      drop(tracker);
      ai::send_entry(b'W', payload);
    }
  }
  Some(room)
}

// ==================== СОКРАЩЕНИЕ НАДПИСИ ====================
//
// Если надпись не помещается, хук сокращает её сам и сразу, в том же
// кадре, а не обрезает по краю: «Профессиональный шахтёр» в колонке на
// 21 знак → «Проф. шахтёр», а не «Профессиональный шахт». Так делают в
// интерфейсах: самое длинное слово сокращается до согласной и получает
// точку, пока надпись не поместится. Обрезка по краю — только если
// сократить словами не получилось. Нейросеть потом всё равно получает
// место (запись W) и может предложить вариант красивее.

/// Гласные в кодировке игры: по таблице кодировки dfint, а без неё —
/// по cp866, в которой работает русский перевод.
fn vowel_table() -> [bool; 256] {
  let mut table = [false; 256];
  for c in "aeiouyAEIOUY".bytes() {
    table[c as usize] = true;
  }
  let utf = &crate::config::CONFIG.encoding.utf;
  for c in "аеёиоуыэюяАЕЁИОУЫЭЮЯ".chars() {
    match utf.get(&(c as u32)) {
      Some(&b) => table[b as usize] = true,
      None => {
        if let Some(b) = cp866(c) {
          table[b as usize] = true;
        }
      }
    }
  }
  table
}

fn cp866(c: char) -> Option<u8> {
  let u = c as u32;
  match u {
    0x410..=0x42f => Some((u - 0x410 + 0x80) as u8),
    0x430..=0x43f => Some((u - 0x430 + 0xa0) as u8),
    0x440..=0x44f => Some((u - 0x440 + 0xe0) as u8),
    0x401 => Some(0xf0),
    0x451 => Some(0xf1),
    _ => None,
  }
}

static VOWELS: LazyLock<[bool; 256]> = LazyLock::new(vowel_table);

pub fn abbreviate(text: &[u8], room: usize) -> Vec<u8> {
  shorten(text, room, &VOWELS)
}

fn is_letter(b: u8) -> bool {
  b.is_ascii_alphabetic() || b >= 0x80
}

/// Сокращение надписи до room знаков (однобайтовая кодировка игры).
pub fn shorten(text: &[u8], room: usize, vowels: &[bool; 256]) -> Vec<u8> {
  let mut words: Vec<Vec<u8>> = text.split(|&b| b == b' ').map(|w| w.to_vec()).collect();
  let join = |w: &Vec<Vec<u8>>| w.join(&b' ');
  let mut done = vec![false; words.len()];
  while join(&words).len() > room {
    // Самое длинное несокращённое слово из шести букв и больше
    let mut at: Option<usize> = None;
    for (i, w) in words.iter().enumerate() {
      let letters = w.iter().filter(|&&b| is_letter(b)).count();
      if done[i] || letters < 6 || w.last() == Some(&b'.') {
        continue;
      }
      let best = at.map(|j| words[j].iter().filter(|&&b| is_letter(b)).count()).unwrap_or(0);
      if letters > best {
        at = Some(i);
      }
    }
    let Some(i) = at else { break };
    done[i] = true;
    let w = &words[i];
    let lead = w.iter().take_while(|&&b| !is_letter(b)).count();
    let end = w.len() - w.iter().rev().take_while(|&&b| !is_letter(b) && b != b'-').count();
    let body = &w[lead..end];
    // До четырёх букв и дальше, пока последняя — гласная: «Проф», «Квал»
    let mut cut = 4.min(body.len().saturating_sub(1));
    while cut < body.len().saturating_sub(2) && vowels[body[cut - 1] as usize] {
      cut += 1;
    }
    let mut short = w[..lead].to_vec();
    short.extend_from_slice(&body[..cut]);
    short.push(b'.');
    words[i] = short;
  }
  let mut out = join(&words);
  if out.len() > room {
    out.truncate(room);
    while out.last() == Some(&b' ') {
      out.pop();
    }
  }
  out
}
