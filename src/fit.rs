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
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(|| {
  Mutex::new(Tracker {
    gen: 0,
    calls: 0,
    mark: None,
    rows: HashMap::new(),
    misses: HashMap::new(),
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
pub fn settle(gps: usize, key: &[u8], drawn: Option<&[u8]>, flow: bool) -> Option<usize> {
  if !ai::EXPERIMENTS.english || ai::EXPERIMENTS.fit_off || gps == 0 || blank(key) {
    return None;
  }
  let (x, y) = ai::cursor(gps);
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
  let neighbor = tracker.room(x, y, text.len());
  let clip = clip_room(gps, x).filter(|&c| key.len() <= c && text.len() > c);
  let room = match (neighbor, clip) {
    (Some(a), Some(b)) => a.min(b),
    (Some(a), None) => a,
    (None, Some(b)) => b,
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
      let mut payload = format!("{room}").into_bytes();
      payload.push(0x1e);
      payload.extend_from_slice(report);
      drop(tracker);
      ai::send_entry(b'W', payload);
    }
  }
  Some(room)
}
