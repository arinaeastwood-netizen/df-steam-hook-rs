//! Нейроперевод: второй словарь и журнал промахов.
//!
//! Хук не обращается к нейросети сам. Он только:
//!   1) записывает строки, которые выводятся на экран без перевода,
//!      в dfint-data/ai/misses.txt — по одной на строку, без повторов;
//!   2) раз в секунду проверяет dfint-data/ai/dictionary_ai.csv и,
//!      если файл изменился, перечитывает его без перезапуска игры.
//!
//! Абзацы. Длинный текст игра сначала создаёт целиком, а потом режет
//! на строки по ширине окна и выводит по одной. Переводить такие куски
//! по отдельности бессмысленно. Поэтому хук держит кольцевой буфер
//! последних длинных строк, которые игра создавала без перевода, и
//! когда на экран попадает кусок одной из них — записывает в журнал
//! исходный абзац целиком. Перевод абзаца подставляется ещё до
//! разрезания, и игра сама переносит русский текст по ширине окна.
//!
//! Формат строки журнала: тип, табуляция, текст.
//!   S — строка с экрана как есть;
//!   P — исходный абзац, из которого взята строка с экрана.
//!
//! Всё остальное (пачки, запросы к API, кэш) делает внешнее приложение.
//! Так сбой сети или переводчика никогда не роняет игру.
//!
//! Код на пути отрисовки никогда не ждёт: блокировки берутся через
//! try_lock / try_send, и при занятости строка просто пропускается —
//! она придёт снова на следующем кадре.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use crate::constants::{PATH_AI_DICTIONARY, PATH_AI_DIR, PATH_AI_MISSES};
use crate::dictionary::Dictionary;

/// Минимальная длина строки, которую имеет смысл переводить.
const MIN_LEN: usize = 2;
/// Слишком длинные строки — почти наверняка служебные буферы.
const MAX_LEN: usize = 4000;
/// Предел уникальных строк за сессию: защита памяти от бесконечного потока.
const MAX_SEEN: usize = 500_000;
/// Ёмкость очереди к потоку записи.
const QUEUE_CAPACITY: usize = 20_000;
/// Период проверки словаря на изменения.
const RELOAD_PERIOD: Duration = Duration::from_millis(1000);
/// Строка короче этого не считается абзацем.
const SOURCE_MIN_LEN: usize = 40;
/// В абзаце должно быть хотя бы столько пробелов.
const SOURCE_MIN_SPACES: usize = 5;
/// Сколько последних длинных строк помнит буфер.
const SOURCE_RING: usize = 512;
/// Кусок короче этого не ищется в абзацах: слишком велик риск
/// найти его в чужом тексте.
const PIECE_MIN_LEN: usize = 4;

static AI_DICTIONARY: LazyLock<RwLock<HashMap<Vec<u8>, Vec<u8>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Число записей во втором словаре. Пока он пуст, поиск в нём
/// пропускается целиком, без взятия блокировки.
static AI_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Последние длинные строки, которые игра создала без перевода.
static SOURCES: LazyLock<Mutex<VecDeque<Vec<u8>>>> = LazyLock::new(|| Mutex::new(VecDeque::with_capacity(SOURCE_RING)));

struct MissState {
  seen: HashSet<Vec<u8>>,
  tx: Option<SyncSender<Vec<u8>>>,
}

static MISSES: LazyLock<Mutex<MissState>> = LazyLock::new(|| {
  Mutex::new(MissState {
    seen: HashSet::new(),
    tx: None,
  })
});

/// Запуск фоновых потоков. Вызывается один раз после установки хуков.
pub fn start() {
  if let Err(err) = fs::create_dir_all(PATH_AI_DIR) {
    log::error!("ai: unable to create {PATH_AI_DIR}: {err}");
    return;
  }

  start_miss_writer();
  start_reloader();

  log::info!("ai: started, dictionary {PATH_AI_DICTIONARY}, misses {PATH_AI_MISSES}");
}

/// Поиск во втором словаре.
pub fn lookup(key: &[u8]) -> Option<Vec<u8>> {
  if AI_SIZE.load(Ordering::Relaxed) == 0 {
    return None;
  }
  match AI_DICTIONARY.try_read() {
    Ok(map) => map.get(key).cloned(),
    Err(_) => None,
  }
}

/// Запоминание длинной строки, созданной игрой без перевода.
/// Вызывается на каждый промах низкоуровневых хуков, поэтому отсев
/// идёт до любых блокировок и копирований.
pub fn remember_source(src: &[u8]) {
  if src.len() < SOURCE_MIN_LEN || src.len() > MAX_LEN {
    return;
  }
  if src.iter().filter(|&&b| b == b' ').count() < SOURCE_MIN_SPACES {
    return;
  }

  let mut ring = match SOURCES.try_lock() {
    Ok(ring) => ring,
    Err(_) => return,
  };

  // Один и тот же текст часто создаётся несколько раз подряд
  if ring.iter().rev().take(8).any(|known| known.as_slice() == src) {
    return;
  }

  if ring.len() >= SOURCE_RING {
    ring.pop_front();
  }
  ring.push_back(src.to_vec());
}

fn trim_spaces(s: &[u8]) -> &[u8] {
  let start = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
  let end = s.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(start);
  &s[start..end]
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
  needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Похожа ли строка с экрана на кусок абзаца. Одиночное слово без
/// знака препинания («Space», «Hotkey») слишком легко найти в чужом
/// тексте, поэтому ищутся только куски с пробелом или концом фразы.
fn looks_like_piece(piece: &[u8]) -> bool {
  if piece.len() < PIECE_MIN_LEN {
    return false;
  }
  let has_space = piece.contains(&b' ');
  let ends_phrase = matches!(piece.last().copied(), Some(b'.' | b',' | b';' | b':' | b'!' | b'?'));
  has_space || ends_phrase
}

/// Поиск абзаца, из которого взята строка с экрана.
fn find_source(line: &[u8]) -> Option<Vec<u8>> {
  let piece = trim_spaces(line);
  if !looks_like_piece(piece) {
    return None;
  }

  let ring = SOURCES.try_lock().ok()?;
  let found = ring
    .iter()
    .rev()
    .find(|src| src.len() > piece.len() + 1 && contains(src, piece))
    .cloned();
  found
}

/// Обрезанная интерфейсом строка («Убийством из S...») — её переводить
/// бессмысленно, полный текст придёт в другом месте.
fn is_truncated(key: &[u8]) -> bool {
  key.ends_with(b"...")
}

/// Регистрация строки без перевода.
pub fn report_miss(key: &[u8]) {
  if !worth_logging(key) || is_truncated(key) {
    return;
  }

  let mut state = match MISSES.try_lock() {
    Ok(state) => state,
    Err(_) => return,
  };

  if state.tx.is_none() || state.seen.len() >= MAX_SEEN || state.seen.contains(key) {
    return;
  }

  let (kind, payload) = match find_source(key) {
    Some(source) => (b'P', source),
    None => (b'S', key.to_vec()),
  };

  // Абзац уже записан по другой его строке — эту просто отмечаем
  if kind == b'P' && state.seen.contains(payload.as_slice()) {
    state.seen.insert(key.to_vec());
    return;
  }

  let mut line = Vec::with_capacity(payload.len() + 2);
  line.push(kind);
  line.push(b'\t');
  line.extend_from_slice(&payload);

  let sent = match state.tx.as_ref() {
    Some(tx) => tx.try_send(line).is_ok(),
    None => false,
  };

  // В «увиденные» строка попадает только после успешной отправки:
  // если очередь была переполнена, она уйдёт в следующий раз
  if sent {
    state.seen.insert(key.to_vec());
    if kind == b'P' {
      state.seen.insert(payload);
    }
  }
}

/// Отсев того, что переводить бессмысленно: короткие обрывки,
/// числа, знаки, строки без латинских букв.
fn worth_logging(key: &[u8]) -> bool {
  if key.len() < MIN_LEN || key.len() > MAX_LEN {
    return false;
  }
  let letters = key.iter().filter(|b| b.is_ascii_alphabetic()).count();
  letters >= 2
}

/// Экранирование для построчного файла: перевод строки внутри
/// строки игры не должен разрывать запись.
fn escape_line(key: &[u8]) -> Vec<u8> {
  let mut out = Vec::with_capacity(key.len() + 2);
  for &b in key {
    match b {
      b'\\' => out.extend_from_slice(b"\\\\"),
      b'\n' => out.extend_from_slice(b"\\n"),
      b'\r' => out.extend_from_slice(b"\\r"),
      0 => {}
      _ => out.push(b),
    }
  }
  out.push(b'\n');
  out
}

fn start_miss_writer() {
  // Файл обнуляется в начале каждой сессии игры. Приложение замечает
  // уменьшение размера и начинает читать файл с начала
  let file = match OpenOptions::new().create(true).write(true).truncate(true).open(PATH_AI_MISSES) {
    Ok(file) => file,
    Err(err) => {
      log::error!("ai: unable to open {PATH_AI_MISSES}: {err}");
      return;
    }
  };

  let (tx, rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = mpsc::sync_channel(QUEUE_CAPACITY);

  match MISSES.lock() {
    Ok(mut state) => state.tx = Some(tx),
    Err(_) => {
      log::error!("ai: miss state is poisoned");
      return;
    }
  }

  std::thread::spawn(move || {
    let mut writer = BufWriter::new(file);
    loop {
      match rx.recv_timeout(Duration::from_millis(500)) {
        Ok(line) => {
          let _ = writer.write_all(&escape_line(&line));
          // Забираем всё, что успело накопиться, и сбрасываем одним блоком
          while let Ok(more) = rx.try_recv() {
            let _ = writer.write_all(&escape_line(&more));
          }
          let _ = writer.flush();
        }
        Err(RecvTimeoutError::Timeout) => {
          let _ = writer.flush();
        }
        Err(RecvTimeoutError::Disconnected) => {
          let _ = writer.flush();
          break;
        }
      }
    }
  });
}

fn modified_at(path: &str) -> Option<SystemTime> {
  fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn start_reloader() {
  std::thread::spawn(move || {
    let mut last: Option<SystemTime> = None;
    loop {
      let current = modified_at(PATH_AI_DICTIONARY);

      if current != last {
        match current {
          Some(_) => match Dictionary::load(PATH_AI_DICTIONARY) {
            Ok(map) => {
              let size = map.len();
              if let Ok(mut guard) = AI_DICTIONARY.write() {
                *guard = map;
                AI_SIZE.store(size, Ordering::Relaxed);
                log::info!("ai: dictionary reloaded, items {size}");
              }
              last = current;
            }
            Err(err) => {
              // Файл мог быть занят записью — повторим на следующем круге
              log::warn!("ai: unable to load dictionary: {err}");
            }
          },
          None => {
            if let Ok(mut guard) = AI_DICTIONARY.write() {
              guard.clear();
              AI_SIZE.store(0, Ordering::Relaxed);
            }
            last = None;
          }
        }
      }

      std::thread::sleep(RELOAD_PERIOD);
    }
  });
}
