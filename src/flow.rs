//! Фразы, которые игра рисует по слову (летопись в режиме «Легенды»).
//!
//! Текст событий легенд игра раскладывает сама: каждое слово выводится
//! отдельным вызовом addst в заранее рассчитанную клетку. Слова словаря
//! сообщества переводятся ещё до раскладки, а нейросетевой перевод
//! подставляется уже при выводе — поэтому имя, ставшее длиннее, налезает
//! на следующие слова («Крапчатаябылатаинственное»), а ставшее короче
//! оставляет дыру. И главное: имя переводится в отрыве от фразы, падеж
//! ему поставить не из чего («поселился в Крапчатая Яма»).
//!
//! Здесь хук узнаёт такие фразы по раскладке: следующее слово стоит на
//! том же ряду ровно через пробел после предыдущего (или с того же края
//! рядом ниже — перенос). Фраза целиком уходит в журнал (тип L), а её
//! перевод хранится в словаре кусками: какой вызов игры рисует какой
//! кусок перевода и в какой клетке. Вызов, которому кусков не досталось,
//! ничего не рисует. Цвет каждого куска — тот, что игра выставила для
//! своего вызова: имя остаётся цветным.
//!
//! Начало фразы видно раньше, чем она вся: игра рисует первое слово, не
//! зная, что за ним. Поэтому фраза запоминается по первому слову и его
//! клетке на прошлом кадре — экран перерисовывается каждый кадр. Если
//! дальше слова не совпали (прокрутка, другое окно), подмена бросается,
//! остаток кадра рисуется как в игре, и со следующего кадра фраза
//! узнаётся заново.
//!
//! Пока перевода фразы нет, работает временная раскладка: слова ряда
//! сдвигаются на разницу длины уже переведённых слов перед ними — имена
//! хотя бы не налезают на соседей.
//!
//! Формат записи журнала L: ширина, затем слова через 0x1F:
//!   «x0,y0,ширина,экран» 0x1F «dx,dy» 0x1E слово 0x1F «dx,dy» 0x1E слово …
//! dx, dy — место слова относительно первого; ширина — самый правый край
//! фраз с того же края экрана, относительно x0; экран — самый правый
//! край всего, что выводилось через addst за сеанс (ширина экрана в
//! клетках: игра не всегда заполняет её фразами).
//!
//! Строка, которая кончается пробелом, — строка абзаца, нарезанного
//! игрой (подсказки, описания в меню): у неё своя сборка (ai::screen),
//! фразой она не считается.
//!
//! В режиме оригинального текста (experiments.txt: english) все слова
//! летописи английские, и каждое отдельно не переводится: слова фразы,
//! кроме слов с заглавной (имён), не попадают в журнал как отдельные
//! строки. Имена попадают — их SkadDF переводит раньше фразы.
//!
//! Ключ словаря: 0x19 и слова фразы через 0x1F. Значение — куски через
//! 0x1F, каждый «номер слова,ряд,столбец» 0x1E текст; ряд и столбец —
//! относительно первого слова.
//!
//! Аварийный выключатель — строка flow_off в dfint-data/ai/experiments.txt.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::ai;

const FLOW_PREFIX: u8 = 0x19;
const SEP: u8 = 0x1f;
const POS_SEP: u8 = 0x1e;

/// Длиннее этого — уже не фраза, а что-то вроде таблицы из слов.
const MAX_TOKENS: usize = 160;
const MAX_TOKEN_LEN: usize = 200;
/// Фраз, запомненных по первому слову. Больше — память начинается заново.
const MAX_KNOWN: usize = 4096;
/// Фраза короче этого в журнал не идёт: пара слов — это подпись и значение.
const MIN_JOURNAL_TOKENS: usize = 3;

struct Tok {
  text: Vec<u8>,
  x: i32,
  y: i32,
}

/// Фраза, увиденная на прошлом кадре: слова и их места от первого слова.
struct Known {
  texts: Vec<Vec<u8>>,
  rel: Vec<(i32, i32)>,
}

/// Кусок перевода: где рисовать (от первого слова) и что.
struct Piece {
  row: i32,
  col: i32,
  text: Vec<u8>,
}

/// Фраза, которая подменяется на этом кадре.
struct Active {
  texts: Vec<Vec<u8>>,
  rel: Vec<(i32, i32)>,
  pieces: Vec<Vec<Piece>>,
  next: usize,
  x0: i32,
  y0: i32,
}

struct State {
  /// Фраза, которая сейчас собирается из выводимых слов
  learn: Vec<Tok>,
  /// Временная раскладка: на сколько сдвинуть следующие слова ряда
  shift: i32,
  shift_row: i32,
  active: Option<Active>,
  known: HashMap<(i32, i32, Vec<u8>), Known>,
  /// Самый правый край фраз по столбцу их начала
  right: HashMap<i32, i32>,
  /// Самый правый край всего выведенного: ширина экрана
  screen_right: i32,
  logged: bool,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| {
  Mutex::new(State {
    learn: Vec::new(),
    shift: 0,
    shift_row: 0,
    active: None,
    known: HashMap::new(),
    right: HashMap::new(),
    screen_right: 0,
    logged: false,
  })
});

/// Что делать с выводом слова.
pub enum Plan {
  /// Вывести как обычно, сдвинув на shift столбцов. quiet — слово внутри
  /// фразы: перевод ищется, но в журнал как отдельная строка не идёт.
  /// flow — слово фразы (продолжение или начало знакомой фразы): у неё
  /// своя раскладка, обрезке надписей (fit.rs) она не подлежит.
  Normal { shift: i32, quiet: bool, flow: bool },
  /// Вместо слова нарисовать эти куски перевода фразы (столбец, ряд, текст).
  /// Пустой список — слово не рисуется вовсе.
  Pieces(Vec<(i32, i32, Vec<u8>)>),
}

fn key_of(texts: &[Vec<u8>]) -> Vec<u8> {
  let size = texts.iter().map(|t| t.len() + 1).sum::<usize>() + 1;
  let mut key = Vec::with_capacity(size);
  key.push(FLOW_PREFIX);
  for (i, t) in texts.iter().enumerate() {
    if i > 0 {
      key.push(SEP);
    }
    key.extend_from_slice(t);
  }
  key
}

fn trim(text: &[u8]) -> &[u8] {
  let start = text.iter().position(|&b| b != b' ').unwrap_or(text.len());
  let end = text.iter().rposition(|&b| b != b' ').map_or(start, |e| e + 1);
  &text[start..end]
}

fn latin_letters(text: &[u8]) -> usize {
  text.iter().filter(|b| b.is_ascii_alphabetic()).count()
}

fn parse_int(bytes: &[u8]) -> Option<i32> {
  std::str::from_utf8(bytes).ok()?.trim().parse::<i32>().ok()
}

/// Значение словаря → куски по номерам слов. Ошибка разбора — None:
/// такая фраза рисуется как в игре.
fn parse_pieces(value: &[u8], count: usize) -> Option<Vec<Vec<Piece>>> {
  let value = match value.last() {
    Some(0) => &value[..value.len() - 1],
    _ => value,
  };
  let mut out: Vec<Vec<Piece>> = (0..count).map(|_| Vec::new()).collect();
  for part in value.split(|&b| b == SEP) {
    if part.is_empty() {
      continue;
    }
    let at = part.iter().position(|&b| b == POS_SEP)?;
    let head = &part[..at];
    let text = &part[at + 1..];
    let mut nums = head.split(|&b| b == b',');
    let index = parse_int(nums.next()?)?;
    let row = parse_int(nums.next()?)?;
    let col = parse_int(nums.next()?)?;
    if index < 0 || index as usize >= count || text.is_empty() {
      continue;
    }
    out[index as usize].push(Piece {
      row,
      col,
      text: text.to_vec(),
    });
  }
  Some(out)
}

/// Фраза закончилась: запомнить её по первому слову и, если перевода
/// ещё нет, записать в журнал.
fn finish(st: &mut State) {
  let toks = std::mem::take(&mut st.learn);
  if toks.len() < 2 {
    return;
  }
  let x0 = toks[0].x;
  let y0 = toks[0].y;

  let edge = toks.iter().map(|t| t.x + t.text.len() as i32).max().unwrap_or(x0);
  let right = st.right.entry(x0).or_insert(edge);
  if edge > *right {
    *right = edge;
  }
  let width = *right - x0;
  let screen = st.screen_right;

  let texts: Vec<Vec<u8>> = toks.iter().map(|t| t.text.clone()).collect();
  let rel: Vec<(i32, i32)> = toks.iter().map(|t| (t.x - x0, t.y - y0)).collect();

  let worth = toks.len() >= MIN_JOURNAL_TOKENS && toks.iter().any(|t| latin_letters(&t.text) >= 2);
  if worth && ai::lookup(&key_of(&texts)).is_none() {
    let mut payload = format!("{x0},{y0},{width},{screen}").into_bytes();
    for (t, (dx, dy)) in toks.iter().zip(rel.iter()) {
      payload.push(SEP);
      payload.extend_from_slice(format!("{dx},{dy}").as_bytes());
      payload.push(POS_SEP);
      payload.extend_from_slice(&t.text);
    }
    ai::send_entry(b'L', payload);
  }

  if st.known.len() >= MAX_KNOWN {
    st.known.clear();
  }
  st.known.insert((x0, y0, texts[0].clone()), Known { texts, rel });
}

/// Начинается ли с этого слова фраза, у которой есть перевод.
fn activate(st: &mut State, x: i32, y: i32, key: &[u8]) -> Option<Vec<(i32, i32, Vec<u8>)>> {
  let known = st.known.get(&(x, y, key.to_vec()))?;
  let value = ai::lookup(&key_of(&known.texts))?;
  let mut pieces = parse_pieces(&value, known.texts.len())?;
  let first = std::mem::take(&mut pieces[0]);
  let active = Active {
    texts: known.texts.clone(),
    rel: known.rel.clone(),
    pieces,
    next: 1,
    x0: x,
    y0: y,
  };
  let out = first.into_iter().map(|p| (x + p.col, y + p.row, p.text)).collect();
  if active.next < active.texts.len() {
    st.active = Some(active);
  }
  Some(out)
}

/// Слово перед выводом: решить, рисовать ли его как обычно или
/// подменить кусками перевода фразы.
pub fn begin(gps: usize, key: &[u8], justify: u8) -> Plan {
  const PLAIN: Plan = Plan::Normal { shift: 0, quiet: false, flow: false };
  if ai::EXPERIMENTS.flow_off || gps == 0 || key.is_empty() || key.len() > MAX_TOKEN_LEN {
    return PLAIN;
  }
  let (x, y) = ai::cursor(gps);
  let mut st = match STATE.try_lock() {
    Ok(st) => st,
    Err(_) => return PLAIN,
  };
  let edge = x + key.len() as i32;
  if edge > st.screen_right && edge < 2000 {
    st.screen_right = edge;
  }

  // Строка абзаца (кончается пробелом, в ней есть буквы) — не слово фразы.
  // В режиме оригинального текста слово летописи — одно слово: надпись
  // из нескольких слов («Stray Yak Cow (Tame)», «Dense fescue grass») —
  // это элемент интерфейса рядом с другими, а не часть фразы
  let several_words = ai::EXPERIMENTS.english && trim(key).contains(&b' ');
  if several_words || (key.last() == Some(&b' ') && key.iter().any(|b| b.is_ascii_alphabetic() || *b >= 0x80)) {
    st.active = None;
    finish(&mut st);
    return PLAIN;
  }

  // Идёт подмена фразы: слово должно быть следующим словом этой фразы
  if let Some(active) = st.active.as_mut() {
    let i = active.next;
    let expected = (active.x0 + active.rel[i].0, active.y0 + active.rel[i].1);
    if active.texts[i] == key && (x, y) == expected {
      active.next += 1;
      let (x0, y0) = (active.x0, active.y0);
      let pieces = std::mem::take(&mut active.pieces[i]);
      if active.next >= active.texts.len() {
        st.active = None;
      }
      return Plan::Pieces(pieces.into_iter().map(|p| (x0 + p.col, y0 + p.row, p.text)).collect());
    }
    // Не совпало: на экране уже другая фраза. Совпавшее начало
    // становится собираемой фразой — со следующего кадра она узнается
    // правильно
    let active = st.active.take().unwrap();
    st.learn = (0..active.next)
      .map(|k| Tok {
        text: active.texts[k].clone(),
        x: active.x0 + active.rel[k].0,
        y: active.y0 + active.rel[k].1,
      })
      .collect();
    st.shift = 0;
    st.shift_row = y;
  }

  // Перенос на следующий ряд — только после ряда из нескольких слов:
  // одиночные надписи столбиком («Nishgan / Tradeddish / Форпост»,
  // «15th Granite / Early Spring / Year 5») — это поля интерфейса, а не
  // фраза, перенесённая у края окна
  let continues = match st.learn.last() {
    Some(last) => {
      let row_words = st.learn.iter().filter(|t| t.y == last.y).count();
      (y == last.y && x == last.x + last.text.len() as i32 + 1)
        || (y == last.y + 1 && x == st.learn[0].x && row_words >= 2)
    }
    None => false,
  };

  if continues && st.learn.len() < MAX_TOKENS {
    if !st.logged {
      st.logged = true;
      log::info!("ai: word flow detected at ({x},{y}), justify={justify}");
    }
    if y != st.shift_row {
      st.shift = 0;
      st.shift_row = y;
    }
    st.learn.push(Tok { text: key.to_vec(), x, y });
    return Plan::Normal { shift: st.shift, quiet: quiet_word(key), flow: true };
  }

  // С этого слова начинается новое: прежняя фраза закончилась
  finish(&mut st);

  if let Some(pieces) = activate(&mut st, x, y, key) {
    return Plan::Pieces(pieces);
  }

  // Начало фразы, которая уже знакома по прошлым кадрам, но ещё не
  // переведена: первое слово тоже не отдельная строка
  let known = st.known.contains_key(&(x, y, key.to_vec()));
  st.learn.push(Tok { text: key.to_vec(), x, y });
  st.shift = 0;
  st.shift_row = y;
  Plan::Normal { shift: 0, quiet: known && quiet_word(key), flow: known }
}

/// Слово фразы, которое не нужно переводить отдельно: в режиме
/// оригинального текста — всё, кроме слов с заглавной (это имена).
fn quiet_word(key: &[u8]) -> bool {
  ai::EXPERIMENTS.english && !key.first().is_some_and(|b| b.is_ascii_uppercase())
}

/// Слово выведено обычным путём: перевод мог изменить его длину, и
/// следующие слова ряда сдвигаются на разницу.
pub fn drawn(original_len: usize, drawn_len: usize) {
  if ai::EXPERIMENTS.flow_off {
    return;
  }
  if let Ok(mut st) = STATE.try_lock() {
    if st.active.is_none() && !st.learn.is_empty() {
      st.shift += drawn_len as i32 - original_len as i32;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pieces_round_trip() {
    let value = b"0,0,0\x1e\xca\xf0\x1f1,0,14\x1e\xe1\xfb\xeb\x1f9,1,3\x1ebad\x00";
    let pieces = parse_pieces(value, 3).unwrap();
    assert_eq!(pieces[0].len(), 1);
    assert_eq!(pieces[1][0].col, 14);
    assert_eq!(pieces[2].len(), 0);
  }

  #[test]
  fn key_layout() {
    let key = key_of(&[b"Speckpit".to_vec(), b"was".to_vec()]);
    assert_eq!(key, b"\x19Speckpit\x1fwas".to_vec());
  }
}
