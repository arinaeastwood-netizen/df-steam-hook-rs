//! Поля ввода игры: поиск, имена, названия.
//!
//! Буквы в поле ввода игра берёт из события SDL_TEXTINPUT: Windows отдаёт
//! окну готовый текст в UTF-8, игра копирует его в буфер на 32 байта, а
//! функция поля ввода (standardstringentry, адрес — в offsets.toml dfint)
//! дописывает в строку поля байт за байтом. Принимает она только
//! латиницу, цифры и пробел, всё остальное молча выбрасывает. Поэтому
//! русская буква до строки поля не доходит.
//!
//! Здесь хук сам разбирает буфер:
//!   — русская буква переводится в байт шрифта игры (cp866 или cp1251,
//!     по encoding.toml установленной сборки dfint);
//!   — латинская буква при русской раскладке окна игры переводится по
//!     клавише («r» → «к»): на случай, если Windows отдала окну латиницу;
//!   — всё остальное фильтруется так же, как это делает сама игра.
//! Готовые буквы дописываются в строку поля функцией самой игры, затем
//! вызывается исходная функция с пустым буфером: она обрабатывает
//! Backspace и Enter и очищает нажатые клавиши, чтобы набранная буква не
//! сработала как горячая клавиша.
//!
//! Кроме того, хук помнит, что сейчас написано в каждом поле. Набранный
//! текст на экране не переводится и не уходит в журнал SkadDF: это ввод
//! игрока, а не текст игры. А пока в поле есть русские буквы, работает
//! русский поиск (search.rs).
//!
//! Первые записи о каждом вводе попадают в dfint-log.log строками
//! «input:»: байты, пришедшие от Windows, и раскладка окна игры.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use crate::config::CONFIG;
use crate::cxxstring::CxxString;

/// Флаги поля ввода — третий аргумент функции игры.
const LETTERS: u8 = 1;
const SPACE: u8 = 2;
const NUMBERS: u8 = 4;
const CAPS: u8 = 8;
const SYMBOLS: u8 = 16;
const FILENAME: u8 = 32;

/// Размер буфера текста SDL в игре.
const TEXT_BUF: usize = 32;
/// Поле, которое не правили и не видели на экране столько времени, забывается.
const FIELD_TTL_MS: u64 = 10 * 60 * 1000;
/// Русский поиск действует, пока поле с русскими буквами видно на экране
/// или правилось не позже этого.
const RU_SEARCH_TTL_MS: u64 = 60 * 1000;
/// Полей одновременно больше не бывает; запас — на случай утечки.
const MAX_FIELDS: usize = 16;
/// Сколько записей «input:» пишется в лог за сеанс.
const LOG_LIMIT: usize = 60;

struct Field {
  text: Vec<u8>,
  /// Текст до последней правки: кадр может быть нарисован раньше, чем
  /// игра обработает ввод
  prev: Vec<u8>,
  seen: u64,
}

static FIELDS: LazyLock<Mutex<HashMap<usize, Field>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static ANY_FIELD: AtomicBool = AtomicBool::new(false);
static RU_SEEN: AtomicU64 = AtomicU64::new(0);
static RU_ACTIVE_LOGGED: AtomicBool = AtomicBool::new(false);
static LOGGED: AtomicUsize = AtomicUsize::new(0);
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Время от запуска, мс. Ноль зарезервирован под «никогда».
fn now_ms() -> u64 {
  STARTED.elapsed().as_millis() as u64 + 1
}

fn has_russian(text: &[u8]) -> bool {
  text.iter().any(|&b| b >= 0x80)
}

/// Буква до записи в строку поля.
enum Item {
  /// Байт шрифта игры для русской буквы
  Game(u8),
  /// Обычный символ ASCII
  Ascii(u8),
}

/// Перед исходной функцией поля ввода. Если в буфере есть русские буквы
/// (или латиница при русской раскладке), дописывает их в строку поля
/// сама и очищает буфер. Возвращает, дописано ли что-нибудь.
pub unsafe fn before_entry(src: *const u8, maxlen: usize, flag: u8, buf: *mut u8) -> bool {
  if src.is_null() || buf.is_null() {
    return false;
  }
  let raw_all = std::slice::from_raw_parts(buf as *const u8, TEXT_BUF);
  let end = raw_all.iter().position(|&b| b == 0).unwrap_or(TEXT_BUF);
  let raw = &raw_all[..end];
  if raw.is_empty() {
    return false;
  }

  let layout = keyboard_layout();
  let cyrillic_layout = is_cyrillic_layout(layout.game);

  // Имена файлов — только латиница, как в игре. Перевод строки — особый
  // случай многострочных полей, его игра обрабатывает сама
  if flag & FILENAME != 0 || raw.contains(&b'\n') {
    log_input(raw, &layout, flag, "game");
    return false;
  }
  let Ok(text) = std::str::from_utf8(raw) else {
    log_input(raw, &layout, flag, "not utf-8, game");
    return false;
  };

  let mut items = Vec::with_capacity(text.len());
  let mut special = false;
  for ch in text.chars() {
    if ch.is_ascii() {
      if cyrillic_layout {
        if let Some(byte) = by_position(ch).and_then(to_game) {
          items.push(Item::Game(byte));
          special = true;
          continue;
        }
      }
      items.push(Item::Ascii(ch as u8));
    } else if let Some(byte) = to_game(ch) {
      items.push(Item::Game(byte));
      special = true;
    }
    // Прочие символы (иероглифы, эмодзи) шрифт игры не покажет — пропускаем
  }

  if !special {
    log_input(raw, &layout, flag, "game");
    return false;
  }

  let content = CxxString::from_ptr(src);
  let room = maxlen.saturating_sub(content.size());
  let mut out = Vec::with_capacity(items.len());
  for item in items {
    if out.len() >= room {
      break;
    }
    match item {
      Item::Game(byte) => {
        if flag & (LETTERS | SYMBOLS) == 0 {
          continue;
        }
        out.push(if flag & CAPS != 0 { CONFIG.encoding.capitalize[byte as usize] } else { byte });
      }
      Item::Ascii(c) => {
        if !ascii_allowed(c, flag) {
          continue;
        }
        out.push(if flag & CAPS != 0 { c.to_ascii_uppercase() } else { c });
      }
    }
  }

  // Исходная функция будет вызвана с пустым буфером: латиница не
  // допишется второй раз, а нажатые клавиши она очистит
  std::ptr::write_bytes(buf, 0, TEXT_BUF);

  if out.is_empty() {
    log_input(raw, &layout, flag, "russian, nothing allowed");
    return false;
  }
  crate::hooks::game_append(src, &out);
  log_input(raw, &layout, flag, if cyrillic_layout { "russian (layout)" } else { "russian" });
  true
}

/// Символ ASCII, который приняла бы сама игра (её проверка в функции
/// поля ввода): с флагом символов — любой; заглавные — всегда;
/// строчные — с флагом букв; пробел и цифры — со своими флагами.
fn ascii_allowed(c: u8, flag: u8) -> bool {
  if c < 0x20 || c == 0x7f {
    return false;
  }
  if flag & SYMBOLS != 0 {
    return true;
  }
  (c.is_ascii_lowercase() && flag & LETTERS != 0)
    || c.is_ascii_uppercase()
    || (c == b' ' && flag & SPACE != 0)
    || (c.is_ascii_digit() && flag & NUMBERS != 0)
}

/// Русская буква в байт шрифта игры: таблица utf из encoding.toml dfint.
/// Ключ таблицы — байты UTF-8 буквы, сложенные в число младшим байтом
/// вперёд («А» = D0 90 → 0x90D0).
fn to_game(ch: char) -> Option<u8> {
  let mut tmp = [0u8; 4];
  let bytes = ch.encode_utf8(&mut tmp).as_bytes();
  let mut key: u32 = 0;
  for (i, &b) in bytes.iter().enumerate() {
    key |= (b as u32) << (8 * i);
  }
  CONFIG.encoding.utf.get(&key).copied()
}

/// Латинская клавиша → русская буква на той же клавише (ЙЦУКЕН).
fn by_position(c: char) -> Option<char> {
  const LOWER: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
  const LOWER_RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
  const UPPER: &str = "QWERTYUIOP{}ASDFGHJKL:\"ZXCVBNM<>~";
  const UPPER_RU: &str = "ЙЦУКЕНГШЩЗХЪФЫВАПРОЛДЖЭЯЧСМИТЬБЮЁ";
  if let Some(i) = LOWER.chars().position(|x| x == c) {
    return LOWER_RU.chars().nth(i);
  }
  if let Some(i) = UPPER.chars().position(|x| x == c) {
    return UPPER_RU.chars().nth(i);
  }
  None
}

/// После исходной функции поля ввода: запоминает, что теперь написано в поле.
pub unsafe fn after_entry(src: *const u8) {
  if src.is_null() {
    return;
  }
  let content = CxxString::from_ptr(src);
  let text = content.to_bytes_without_nul().to_vec();
  let now = now_ms();
  let russian = has_russian(&text);

  let Ok(mut fields) = FIELDS.lock() else {
    return;
  };
  fields.retain(|_, f| now.saturating_sub(f.seen) < FIELD_TTL_MS);
  if fields.len() >= MAX_FIELDS && !fields.contains_key(&(src as usize)) {
    fields.clear();
  }
  let field = fields.entry(src as usize).or_insert(Field {
    text: Vec::new(),
    prev: Vec::new(),
    seen: now,
  });
  if field.text != text {
    field.prev = std::mem::replace(&mut field.text, text);
  }
  field.seen = now;
  ANY_FIELD.store(true, Ordering::Relaxed);
  drop(fields);

  if russian {
    RU_SEEN.store(now, Ordering::Relaxed);
  }
  note_russian_search();
}

/// Строка на экране — текст поля ввода (с курсором «_» на конце или без).
/// Такую строку хук не переводит и не записывает в журнал.
pub fn is_entry_text(key: &[u8]) -> bool {
  if !ANY_FIELD.load(Ordering::Relaxed) || key.is_empty() {
    return false;
  }
  let Ok(mut fields) = FIELDS.try_lock() else {
    return false;
  };
  let stripped = key.strip_suffix(b"_").unwrap_or(key);
  let now = now_ms();
  for field in fields.values_mut() {
    let same = |t: &Vec<u8>| !t.is_empty() && (t.as_slice() == key || t.as_slice() == stripped);
    if same(&field.text) || same(&field.prev) {
      field.seen = now;
      if has_russian(&field.text) {
        RU_SEEN.store(now, Ordering::Relaxed);
      }
      return true;
    }
  }
  false
}

/// Идёт русский поиск: поле с русскими буквами недавно правили или видели
/// на экране. Только тогда к названиям добавляется русский ключ (search.rs).
pub fn russian_search_active() -> bool {
  let seen = RU_SEEN.load(Ordering::Relaxed);
  seen != 0 && now_ms().saturating_sub(seen) < RU_SEARCH_TTL_MS
}

fn note_russian_search() {
  let active = russian_search_active();
  if RU_ACTIVE_LOGGED.swap(active, Ordering::Relaxed) != active {
    log::info!("input: russian search {}", if active { "on" } else { "off" });
  }
}

// ==================== РАСКЛАДКА ====================

struct Layout {
  /// Язык раскладки потока игры — по ней Windows переводит клавиши в буквы
  game: u16,
  /// Язык раскладки окна на переднем плане
  foreground: u16,
}

#[cfg(target_os = "windows")]
fn keyboard_layout() -> Layout {
  use winapi::um::winuser::{GetForegroundWindow, GetKeyboardLayout, GetWindowThreadProcessId};
  unsafe {
    let game = (GetKeyboardLayout(0) as usize & 0xffff) as u16;
    let window = GetForegroundWindow();
    let foreground = if window.is_null() {
      0
    } else {
      let thread = GetWindowThreadProcessId(window, std::ptr::null_mut());
      if thread == 0 {
        0
      } else {
        (GetKeyboardLayout(thread) as usize & 0xffff) as u16
      }
    };
    Layout { game, foreground }
  }
}

#[cfg(not(target_os = "windows"))]
fn keyboard_layout() -> Layout {
  Layout { game: 0, foreground: 0 }
}

/// Русская, украинская, белорусская или казахская раскладка.
fn is_cyrillic_layout(lang: u16) -> bool {
  matches!(lang & 0x3ff, 0x19 | 0x22 | 0x23 | 0x3f)
}

fn log_input(raw: &[u8], layout: &Layout, flag: u8, result: &str) {
  if LOGGED.fetch_add(1, Ordering::Relaxed) >= LOG_LIMIT {
    return;
  }
  let hex: Vec<String> = raw.iter().map(|b| format!("{b:02X}")).collect();
  log::info!(
    "input: bytes [{}] layout game={:04x} foreground={:04x} flags={:#04x} -> {}",
    hex.join(" "),
    layout.game,
    layout.foreground,
    flag,
    result
  );
}
