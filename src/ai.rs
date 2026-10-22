//! Нейроперевод: второй словарь и журнал промахов.
//!
//! Хук не обращается к нейросети сам. Он только:
//!   1) записывает строки, которые выводятся на экран без перевода,
//!      в dfint-data/ai/misses.txt — по одной на строку, без повторов;
//!   2) раз в секунду проверяет dfint-data/ai/dictionary_ai.csv и,
//!      если файл изменился, перечитывает его без перезапуска игры.
//!
//! Абзацы. Длинный текст (подсказки, описания) игра сама режет на
//! строки по ширине окна и выводит их подряд. Полный текст при этом
//! не проходит через перехватываемые функции — поэтому такие подсказки
//! не переводит и словарь dfint, хотя перевод в нём есть.
//!
//! У разрезанного текста есть надёжная примета: каждая строка, кроме
//! последней, заканчивается пробелом. Хук склеивает подряд идущие
//! строки экрана в абзац, пока не встретит строку без пробела в конце.
//!
//! Перевод абзаца хранится построчно с контекстом: ключ строки — это
//! все строки абзаца от первой до неё самой через разделитель 0x1F,
//! с разделителем в начале. Так одинаковая строка «orders.» в разных
//! подсказках получает разный перевод, а ключи абзацев никогда не
//! совпадают с обычными ключами словаря.
//!
//! Абзацы и строки целиком на русском тоже записываются: игра склеивает
//! их из отдельно переведённых слов, и согласование бывает сломано
//! («Это элементальный звездный пыльный кувалда»). Готовые переводы
//! сообщества приложение отсеивает само, без запросов к нейросети.
//! Строка с «s» множественного числа после русского слова («74 Годs»)
//! записывается даже с цифрами — иначе её не исправить.
//! Обрезанные интерфейсом строки («…maelstrom mast...») записываются:
//! приложение переводит их с той же длиной и многоточием на конце.
//!
//! Формат записи журнала: тип, табуляция, текст.
//!   # — метка сеанса, всегда первая строка файла;
//!   U — ключ из словаря нейроперевода, который пригодился в этом сеансе;
//!   D — диагностика (эксперимент screen_probe): функция вывода, первые
//!       четыре числа структуры графики игры и сам текст;
//!   S — строка с экрана как есть;
//!   R — абзац: его строки через 0x1F, в точности как на экране.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use crate::constants::{PATH_AI_DICTIONARY, PATH_AI_DIR, PATH_AI_EXPERIMENTS, PATH_AI_MISSES};
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
/// Разделитель строк абзаца в ключах словаря и в журнале.
const LINE_SEPARATOR: u8 = 0x1f;
/// Абзац длиннее этого числа строк — почти наверняка не абзац,
/// а цепочка несвязанных надписей, оканчивающихся пробелом.
const RUN_MAX_LINES: usize = 40;
const RUN_MAX_BYTES: usize = 4000;
/// Меньше двух строк — не абзац. Пределы подобраны так, чтобы
/// короткие подсказки вроде «Finish placing » + «structures.» были
/// абзацем, а «Hotkey: » + «m» — нет.
const PARAGRAPH_MIN_LINES: usize = 2;
const PARAGRAPH_MIN_LEN: usize = 16;
const PARAGRAPH_MIN_SPACES: usize = 2;
/// Русская фраза длиннее этого — уже не склейка игры, а текст.
const RUSSIAN_MAX_LEN: usize = 200;

/// Запись второго словаря. used — перевод уже пригодился после
/// последней перезагрузки словаря: об использовании сообщается один
/// раз, а не на каждом кадре.
struct AiEntry {
  value: Vec<u8>,
  used: AtomicBool,
}

static AI_DICTIONARY: LazyLock<RwLock<HashMap<Vec<u8>, AiEntry>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Число записей во втором словаре. Пока он пуст, поиск в нём
/// пропускается целиком, без взятия блокировки.
static AI_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Строки абзаца, который сейчас выводится на экран.
/// missed — у строки не нашлось никакого перевода.
struct Run {
  lines: Vec<Vec<u8>>,
  missed: Vec<bool>,
  bytes: usize,
}

static RUN: LazyLock<Mutex<Run>> = LazyLock::new(|| {
  Mutex::new(Run {
    lines: Vec::new(),
    missed: Vec::new(),
    bytes: 0,
  })
});

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

// ==================== ЭКСПЕРИМЕНТЫ ====================

/// Эксперименты включаются строками в dfint-data/ai/experiments.txt,
/// файл пишет приложение. Читается один раз при запуске игры.
///   addst_flag_off — выключить перехват addst_flag (он рисует вкладки
///                    и кнопки и теперь включён всегда) — на случай вылетов;
///   screen_probe   — запись для каждой надписи её позиции и цвета.
pub struct Experiments {
  /// Перехват addst_flag теперь включён всегда; эта строка в файле —
  /// аварийный выключатель на случай вылетов
  pub addst_flag_off: bool,
  pub screen_probe: bool,
}

pub static EXPERIMENTS: LazyLock<Experiments> = LazyLock::new(|| {
  let text = fs::read_to_string(PATH_AI_EXPERIMENTS).unwrap_or_default();
  let has = |name: &str| text.lines().any(|l| l.trim() == name);
  Experiments {
    addst_flag_off: has("addst_flag_off"),
    screen_probe: has("screen_probe"),
  }
});

/// Сколько диагностических записей пишется за сеанс: с запасом на
/// несколько экранов, но без риска забить диск при прокрутке карты.
const PROBE_LIMIT: usize = 30_000;
static PROBED: AtomicUsize = AtomicUsize::new(0);

/// Поля структуры графики игры, найденные разбором кода addst и вывода
/// символа в версии 52.05: позиция курсора и текущий цвет.
const GPS_SCREENX: usize = 0x84;
const GPS_SCREENY: usize = 0x88;
const GPS_SCREENF: usize = 0x8c;
const GPS_SCREENBRIGHT: usize = 0x8e;

/// Позиция, с которой будет нарисован следующий текст.
pub fn cursor(gps: usize) -> (i32, i32) {
  if gps == 0 {
    return (0, 0);
  }
  unsafe {
    (
      std::ptr::read_unaligned((gps + GPS_SCREENX) as *const i32),
      std::ptr::read_unaligned((gps + GPS_SCREENY) as *const i32),
    )
  }
}

/// Диагностика: функция вывода (A — addst, T — addst_top, F — addst_flag,
/// C — addcoloredst), позиция на экране, цвет и текст. Одинаковый текст
/// в том же месте экрана пишется один раз.
pub fn probe(func: u8, gps: usize, key: &[u8]) {
  if !EXPERIMENTS.screen_probe || gps == 0 || PROBED.load(Ordering::Relaxed) >= PROBE_LIMIT {
    return;
  }
  let (x, y) = cursor(gps);
  let (fg, bright) = unsafe {
    (
      std::ptr::read_unaligned((gps + GPS_SCREENF) as *const i8),
      std::ptr::read_unaligned((gps + GPS_SCREENBRIGHT) as *const i8),
    )
  };
  let mut payload = format!("{}\t{}\t{}\t{}\t{}\t", func as char, x, y, fg, bright).into_bytes();
  payload.extend_from_slice(key);
  if send_entry(b'D', payload) {
    PROBED.fetch_add(1, Ordering::Relaxed);
  }
}

/// Запуск фоновых потоков. Вызывается один раз после установки хуков.
pub fn start() {
  if let Err(err) = fs::create_dir_all(PATH_AI_DIR) {
    log::error!("ai: unable to create {PATH_AI_DIR}: {err}");
    return;
  }

  start_miss_writer();
  start_reloader();

  log::info!("ai: started, dictionary {PATH_AI_DICTIONARY}, misses {PATH_AI_MISSES}");
  log::info!(
    "ai: experiments: addst_flag_off={}, screen_probe={}",
    EXPERIMENTS.addst_flag_off,
    EXPERIMENTS.screen_probe
  );
}

/// Поиск во втором словаре.
pub fn lookup(key: &[u8]) -> Option<Vec<u8>> {
  if AI_SIZE.load(Ordering::Relaxed) == 0 {
    return None;
  }
  let map = AI_DICTIONARY.try_read().ok()?;
  let entry = map.get(key)?;
  let value = entry.value.clone();

  // Первое использование перевода после загрузки словаря — сообщаем
  // приложению: по этим отметкам оно убирает в архив переводы, которые
  // игра давно не показывала (имена погибших, названия старых миров)
  if !entry.used.swap(true, Ordering::Relaxed) {
    send_entry(b'U', key.to_vec());
  }
  Some(value)
}

/// Строка, после которой абзац продолжается: заканчивается пробелом
/// и содержит буквы.
fn continues_paragraph(key: &[u8]) -> bool {
  key.last() == Some(&b' ') && key.iter().any(|b| b.is_ascii_alphabetic())
}

/// Ключ строки абзаца: разделитель, затем все строки до текущей
/// включительно через разделитель.
fn context_key(lines: &[Vec<u8>]) -> Vec<u8> {
  let size = lines.iter().map(|l| l.len() + 1).sum::<usize>();
  let mut key = Vec::with_capacity(size);
  for line in lines {
    key.push(LINE_SEPARATOR);
    key.extend_from_slice(line);
  }
  key
}

/// Обычный поиск строки с записью промаха в журнал.
fn plain_or_miss(key: &[u8], plain: fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
  match plain(key) {
    Some(value) => Some(value),
    None => {
      report_miss(key);
      None
    }
  }
}

/// Строка, выводимая на экран: сборка абзаца, перевод с контекстом,
/// запись промахов. plain — обычный поиск по словарям.
pub fn screen(key: &[u8], plain: fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
  let continues = continues_paragraph(key);

  let mut run = match RUN.try_lock() {
    Ok(run) => run,
    // Состояние занято — без сборки абзаца, но и без задержки кадра
    Err(_) => return plain_or_miss(key, plain),
  };

  // Самый частый случай: обычная надпись вне абзаца
  if run.lines.is_empty() && !continues {
    drop(run);
    return plain_or_miss(key, plain);
  }

  // Цепочка разрослась сверх меры — это не абзац; сбрасываем её
  let overflow = if run.lines.len() >= RUN_MAX_LINES || run.bytes + key.len() > RUN_MAX_BYTES {
    let lines = std::mem::take(&mut run.lines);
    let missed = std::mem::take(&mut run.missed);
    run.bytes = 0;
    Some((lines, missed))
  } else {
    None
  };

  run.lines.push(key.to_vec());
  run.bytes += key.len();

  let in_context = if AI_SIZE.load(Ordering::Relaxed) > 0 {
    lookup(&context_key(&run.lines))
  } else {
    None
  };
  let result = match in_context {
    Some(value) => Some(value),
    None => plain(key),
  };
  run.missed.push(result.is_none());

  let finished = if continues {
    None
  } else {
    let lines = std::mem::take(&mut run.lines);
    let missed = std::mem::take(&mut run.missed);
    run.bytes = 0;
    Some((lines, missed))
  };

  drop(run);

  if let Some((lines, missed)) = overflow {
    finish_lines(&lines, &missed);
  }
  if let Some((lines, missed)) = finished {
    finish_run(lines, missed);
  }

  result
}

/// Строки, не сложившиеся в абзац, записываются как обычные промахи.
fn finish_lines(lines: &[Vec<u8>], missed: &[bool]) {
  for (line, &miss) in lines.iter().zip(missed.iter()) {
    if miss {
      report_miss(line);
    }
  }
}

/// Подпись и значение: «World: » + название мира, «Hotkey: » + «m».
/// Две строки, первая заканчивается двоеточием — это не текст.
fn is_label_pair(lines: &[Vec<u8>]) -> bool {
  if lines.len() != 2 {
    return false;
  }
  let first = &lines[0];
  let end = first.iter().rposition(|&b| b != b' ');
  matches!(end.map(|i| first[i]), Some(b':'))
}

/// Завершение абзаца: если это настоящий абзац и в нём есть строки
/// без перевода — в журнал уходит весь абзац с разбивкой по строкам.
fn finish_run(lines: Vec<Vec<u8>>, missed: Vec<bool>) {
  let total: usize = lines.iter().map(|l| l.len()).sum();
  let spaces: usize = lines.iter().map(|l| l.iter().filter(|&&b| b == b' ').count()).sum();
  let is_paragraph = lines.len() >= PARAGRAPH_MIN_LINES
    && total >= PARAGRAPH_MIN_LEN
    && spaces >= PARAGRAPH_MIN_SPACES
    && !is_label_pair(&lines);

  if !is_paragraph {
    finish_lines(&lines, &missed);
    return;
  }
  if !missed.iter().any(|&m| m) {
    return;
  }

  // Русский абзац с цифрами — дата, счётчик, возраст: такой текст
  // меняется постоянно, и переводить его как отдельную строку бессмысленно
  let has_latin = lines.iter().any(|l| l.iter().any(|b| b.is_ascii_alphabetic()));
  let has_digits = lines.iter().any(|l| l.iter().any(|b| b.is_ascii_digit()));
  if !has_latin && has_digits {
    return;
  }

  let mut joined = Vec::with_capacity(total + lines.len());
  for (i, line) in lines.iter().enumerate() {
    if i > 0 {
      joined.push(LINE_SEPARATOR);
    }
    joined.extend_from_slice(line);
  }
  send_entry(b'R', joined);
}

/// Отправка записи в журнал с проверкой повторов.
fn send_entry(kind: u8, payload: Vec<u8>) -> bool {
  let mut state = match MISSES.try_lock() {
    Ok(state) => state,
    Err(_) => return false,
  };

  // Повторы отсекаются с учётом типа записи: строка, которая в начале
  // сеанса пришла без перевода (S), после перевода придёт ещё раз как
  // использованная (U) — и это разные события
  let mut tagged = Vec::with_capacity(payload.len() + 1);
  tagged.push(kind);
  tagged.extend_from_slice(&payload);

  if state.tx.is_none() || state.seen.len() >= MAX_SEEN || state.seen.contains(tagged.as_slice()) {
    return false;
  }

  let mut line = Vec::with_capacity(payload.len() + 2);
  line.push(kind);
  line.push(b'\t');
  line.extend_from_slice(&payload);

  let sent = match state.tx.as_ref() {
    Some(tx) => tx.try_send(line).is_ok(),
    None => false,
  };

  // В «увиденные» запись попадает только после успешной отправки:
  // если очередь была переполнена, она уйдёт в следующий раз
  if sent {
    state.seen.insert(tagged);
  }
  sent
}

/// Регистрация строки без перевода.
pub fn report_miss(key: &[u8]) {
  if !worth_logging(key) {
    return;
  }
  send_entry(b'S', key.to_vec());
}

/// Отсев того, что переводить бессмысленно.
///
/// Строка с латиницей записывается, если в ней хотя бы две латинские
/// буквы: короткие обрывки и числа не нужны.
///
/// Строка с русскими буквами (байты от 0x80 в кодировке игры) — это
/// текст, который уже перевёл словарь dfint. Он записывается в двух
/// случаях, когда перевод сломан:
///   — на конце одна английская буква: игра приписала «s» множественного
///     числа к переведённому слову («Браслетs»);
///   — несколько слов без цифр: игра склеила фразу из отдельно
///     переведённых слов, и род с падежом могут не совпадать
///     («Густой куропаточья трава»).
/// Строки с цифрами пропускаются: счётчики и даты меняются постоянно.
fn worth_logging(key: &[u8]) -> bool {
  if key.len() < MIN_LEN || key.len() > MAX_LEN {
    return false;
  }

  let letters = key.iter().filter(|b| b.is_ascii_alphabetic()).count();
  let high = key.iter().filter(|&&b| b >= 0x80).count();

  if high == 0 {
    return letters >= 2;
  }
  if letters >= 2 {
    return true;
  }

  // «s» множественного числа после русского слова — записывается даже
  // со счётчиком: «74 Годs , ♀» нужно исправить на «74 года»
  if has_plural_s(key) {
    return true;
  }

  if key.iter().any(|b| b.is_ascii_digit()) || high < 2 {
    return false;
  }

  let n = key.len();
  let plural_suffix = n >= 2 && key[n - 1].is_ascii_alphabetic() && key[n - 2] >= 0x80;
  let several_words = letters == 0 && key.len() <= RUSSIAN_MAX_LEN && trim_spaces(key).contains(&b' ');

  plural_suffix || several_words
}

/// Русская буква, за ней «s», за ней не буква: «Годs », «Браслетs».
fn has_plural_s(key: &[u8]) -> bool {
  key.windows(2).enumerate().any(|(i, w)| {
    w[0] >= 0x80 && w[1] == b's' && key.get(i + 2).map_or(true, |b| !b.is_ascii_alphabetic())
  })
}

fn trim_spaces(s: &[u8]) -> &[u8] {
  let start = s.iter().position(|&b| b != b' ').unwrap_or(s.len());
  let end = s.iter().rposition(|&b| b != b' ').map(|i| i + 1).unwrap_or(start);
  &s[start..end]
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

    // Метка сеанса в первой строке: по ней приложение отличает новый
    // журнал от старого, даже если новый успел вырасти больше старого
    let stamp = SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map(|d| d.as_nanos())
      .unwrap_or(0);
    let _ = writer.write_all(format!("#\t{stamp}\n").as_bytes());
    let _ = writer.flush();

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
              let map: HashMap<Vec<u8>, AiEntry> = map
                .into_iter()
                .map(|(k, v)| {
                  (
                    k,
                    AiEntry {
                      value: v,
                      used: AtomicBool::new(false),
                    },
                  )
                })
                .collect();
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
