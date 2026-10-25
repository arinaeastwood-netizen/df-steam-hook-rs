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
//!   G — надпись в окне дварфийки, для которой нужна женская форма;
//!   Q — блок цветного текста: строки через 0x1F, у каждой после 0x1E
//!       цвета букв шестнадцатеричными цифрами;
//!   R — абзац: его строки через 0x1F, в точности как на экране;
//!   L — фраза, которую игра рисует по слову (летопись): см. flow.rs.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use crate::constants::{PATH_AI_CORE, PATH_AI_DICTIONARY, PATH_AI_DIR, PATH_AI_EXPERIMENTS, PATH_AI_MISSES, PATH_AI_UI};
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

/// Базовый словарь (dictionary_ai_core.csv). До версии 13 хук применял
/// нейросетевой перевод и при копировании строк, и переведённые имена
/// попадали в сохранения. Эти значения заморожены: хук применяет их
/// везде, как раньше, — тогда игра одинаково видит и сохранённые имена,
/// и свои собственные, и сохранение загружается.
static AI_CORE: LazyLock<RwLock<HashMap<Vec<u8>, Vec<u8>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static CORE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Надписи интерфейса (dictionary_ai_ui.csv) — только в режиме
/// оригинального текста. Применяются там, где игра собирает строки,
/// вместо словаря сообщества: кнопки центрируются по русской длине.
/// Приложение кладёт сюда лишь надписи из обычных английских слов —
/// ни имён мира, ни имён слов языков.
static AI_UI: LazyLock<RwLock<HashMap<Vec<u8>, Vec<u8>>>> = LazyLock::new(|| RwLock::new(HashMap::new()));
static UI_SIZE: AtomicUsize = AtomicUsize::new(0);
/// Русская надпись словаря интерфейса → длина английского оригинала:
/// для выравнивания надписей, которые игра собрала уже русскими (align.rs)
static UI_SOURCE_LEN: LazyLock<RwLock<HashMap<Vec<u8>, usize>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Длина английского оригинала для русской надписи из словаря интерфейса.
pub fn ui_source_len(drawn: &[u8]) -> Option<usize> {
  if UI_SIZE.load(Ordering::Relaxed) == 0 {
    return None;
  }
  UI_SOURCE_LEN.try_read().ok()?.get(drawn).copied()
}

/// Поиск надписи интерфейса для сборки строк (режим оригинального текста).
pub fn lookup_ui(key: &[u8]) -> Option<Vec<u8>> {
  if UI_SIZE.load(Ordering::Relaxed) == 0 || is_word_key(key) {
    return None;
  }
  AI_UI.try_read().ok()?.get(key).cloned()
}

fn load_ui() -> Option<SystemTime> {
  let stamp = modified_at(PATH_AI_UI);
  if stamp.is_none() {
    if let Ok(mut guard) = AI_UI.write() {
      guard.clear();
      UI_SIZE.store(0, Ordering::Relaxed);
    }
    return None;
  }
  match Dictionary::load(PATH_AI_UI) {
    Ok(map) => {
      let size = map.len();
      let reverse: HashMap<Vec<u8>, usize> = map
        .iter()
        .map(|(k, v)| (v[..v.len().saturating_sub(1)].to_vec(), k.len()))
        .collect();
      if let Ok(mut guard) = UI_SOURCE_LEN.write() {
        *guard = reverse;
      }
      if let Ok(mut guard) = AI_UI.write() {
        *guard = map;
        UI_SIZE.store(size, Ordering::Relaxed);
        log::info!("ai: interface dictionary loaded, items {size}");
      }
      stamp
    }
    // Файл мог быть занят записью — повторим на следующем круге
    Err(_) => None,
  }
}

/// Поиск в базовом словаре. Блокировка берётся с ожиданием, а не через
/// try_lock: здесь пропуск перевода означал бы несовпадение имён, а
/// запись в базовый словарь бывает лишь при его перезагрузке.
pub fn lookup_core(key: &[u8]) -> Option<Vec<u8>> {
  if CORE_SIZE.load(Ordering::Relaxed) == 0 {
    return None;
  }
  match AI_CORE.read() {
    Ok(map) => map.get(key).cloned(),
    Err(_) => None,
  }
}

fn load_core() -> Option<SystemTime> {
  let stamp = modified_at(PATH_AI_CORE);
  match Dictionary::load(PATH_AI_CORE) {
    Ok(map) => {
      let size = map.len();
      if let Ok(mut guard) = AI_CORE.write() {
        *guard = map;
        CORE_SIZE.store(size, Ordering::Relaxed);
        log::info!("ai: core dictionary loaded, items {size}");
      }
      stamp
    }
    Err(err) => {
      // Файл исчез — словаря больше нет. Файл есть, но не прочитался
      // (занят записью) — прежние значения остаются: пропажа базового
      // перевода посреди игры означала бы несовпадение имён
      if fs::metadata(PATH_AI_CORE).is_err() {
        if let Ok(mut guard) = AI_CORE.write() {
          guard.clear();
          CORE_SIZE.store(0, Ordering::Relaxed);
        }
      } else {
        log::warn!("ai: unable to load core dictionary: {err}");
      }
      None
    }
  }
}

/// Число записей во втором словаре. Пока он пуст, поиск в нём
/// пропускается целиком, без взятия блокировки.
static AI_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Строки абзаца, который сейчас выводится на экран.
/// missed — у строки не нашлось никакого перевода.
/// x0 — с какого столбца начинается абзац, last_y — ряд последней строки.
/// Следующая строка абзаца стоит ровно рядом ниже с того же края: так
/// перенос строки отличается от надписи, нарисованной в другом месте
/// (например, «Кликните, чтобы вернуть…» на десять рядов ниже текста).
struct Run {
  lines: Vec<Vec<u8>>,
  missed: Vec<bool>,
  bytes: usize,
  x0: i32,
  last_y: i32,
}

static RUN: LazyLock<Mutex<Run>> = LazyLock::new(|| {
  Mutex::new(Run {
    lines: Vec::new(),
    missed: Vec::new(),
    bytes: 0,
    x0: 0,
    last_y: 0,
  })
});

type RunParts = (Vec<Vec<u8>>, Vec<bool>);

fn take_run(run: &mut Run) -> RunParts {
  let lines = std::mem::take(&mut run.lines);
  let missed = std::mem::take(&mut run.missed);
  run.bytes = 0;
  (lines, missed)
}

// ==================== ПОЛ В ОКНЕ ====================
//
// В окне дварфа рядом с возрастом рисуется значок пола: ♀ (байт 0x0C в
// шрифте игры) или ♂ (0x0B). Надписи того же окна — «Здоров»,
// «Болтливый» — игра выводит одинаково для обоих полов. Пока значок ♀
// виден (окно перерисовывается каждый кадр), хук ищет для надписей
// женскую форму: ключ с приставкой 0x1B 'f'.

const GENDER_NONE: u8 = 0;
const GENDER_FEMALE: u8 = 1;
const GENDER_MALE: u8 = 2;
/// Сколько миллисекунд значок пола действует после того, как нарисован.
const GENDER_TTL_MS: u64 = 150;

static GENDER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(GENDER_NONE);
static GENDER_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STARTED: LazyLock<std::time::Instant> = LazyLock::new(std::time::Instant::now);

fn now_ms() -> u64 {
  STARTED.elapsed().as_millis() as u64
}

fn note_gender(key: &[u8]) {
  let gender = if key.contains(&0x0c) {
    GENDER_FEMALE
  } else if key.contains(&0x0b) {
    GENDER_MALE
  } else {
    return;
  };
  GENDER.store(gender, Ordering::Relaxed);
  GENDER_AT.store(now_ms(), Ordering::Relaxed);
}

fn female_context() -> bool {
  GENDER.load(Ordering::Relaxed) == GENDER_FEMALE
    && now_ms().saturating_sub(GENDER_AT.load(Ordering::Relaxed)) <= GENDER_TTL_MS
}

/// Надпись, у которой может быть женская форма: без цифр, с буквами,
/// не больше четырёх слов — или фраза в кавычках («"I've been rained
/// on.  It's annoying."»): так в окне дварфа выводится его речь.
fn gender_candidate(key: &[u8]) -> bool {
  let inner = trim_spaces(key);
  let quote = inner.first() == Some(&b'"');
  let limit = if quote { 200 } else { 60 };
  if inner.len() < 3 || inner.len() > limit || inner.iter().any(|b| b.is_ascii_digit()) {
    return false;
  }
  if inner.contains(&0x0c) || inner.contains(&0x0b) {
    return false;
  }
  let letters = inner.iter().filter(|&&b| b.is_ascii_alphabetic() || b >= 0x80).count();
  let words = inner.split(|&b| b == b' ').filter(|w| !w.is_empty()).count();
  letters >= 3 && (words <= 4 || (quote && words <= 30))
}

/// Обычный поиск с учётом пола: при видимом ♀ сначала женская форма.
/// Надпись, для которой женской формы ещё нет, записывается в журнал
/// (тип G) — приложение решит, меняется ли она.
fn plain_gendered(key: &[u8], plain: fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
  if female_context() && gender_candidate(key) {
    let mut gkey = Vec::with_capacity(key.len() + 2);
    gkey.extend_from_slice(&[0x1b, b'f']);
    gkey.extend_from_slice(key);
    if let Some(value) = lookup(&gkey) {
      return Some(value);
    }
    send_entry(b'G', key.to_vec());
  }
  plain(key)
}

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
  /// Аварийный выключатель перехвата склейки «константа + строка»
  pub cstr_plus_off: bool,
  /// Аварийный выключатель перевода цветного текста (addcoloredst)
  pub colored_off: bool,
  /// Аварийный выключатель перевода фраз, которые игра рисует по слову (flow.rs)
  pub flow_off: bool,
  /// Оригинальный текст: словарь сообщества не подставляется нигде — ни
  /// при сборке строк, ни при выводе. Игра выводит английский текст
  /// (имена, летопись, интерфейс), его переводит SkadDF. Базовый
  /// замороженный словарь остаётся: он нужен старым сохранениям.
  pub english: bool,
  /// Аварийный выключатель обрезки надписей, которым не хватает места (fit.rs)
  pub fit_off: bool,
  /// Аварийный выключатель выравнивания надписей по центру и краю (align.rs)
  pub align_off: bool,
  pub screen_probe: bool,
}

pub static EXPERIMENTS: LazyLock<Experiments> = LazyLock::new(|| {
  let text = fs::read_to_string(PATH_AI_EXPERIMENTS).unwrap_or_default();
  let has = |name: &str| text.lines().any(|l| l.trim() == name);
  Experiments {
    addst_flag_off: has("addst_flag_off"),
    cstr_plus_off: has("cstr_plus_off"),
    colored_off: has("colored_off"),
    flow_off: has("flow_off"),
    english: has("english"),
    fit_off: has("fit_off"),
    align_off: has("align_off"),
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

/// Связки, которыми игра соединяет части описания характера.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CONNECTORS: [&[u8]; 4] = [b", and ", b", though ", b", although ", b", even though "];

/// Можно ли переводить кусок, который игра склеивает со строкой.
///
/// Через ту же склейку игра собирает и текст для экрана, и служебные
/// имена при загрузке мира. Перевод служебного куска («'s », «thick »)
/// ломает имя, по которому игра ищет слово, — загрузка падает с ошибкой
/// «Missing Word Definition». Поэтому переводятся только фразы: не
/// меньше двух пробелов внутри, только печатные латинские символы, —
/// и несколько связок описания характера.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn concat_fragment_allowed(key: &[u8]) -> bool {
  if CONNECTORS.contains(&key) {
    return true;
  }
  if !key.iter().all(|&b| (0x20..0x7f).contains(&b)) {
    return false;
  }
  let inner = trim_spaces(key);
  let spaces = inner.iter().filter(|&&b| b == b' ').count();
  spaces >= 2 && inner.iter().any(|b| b.is_ascii_lowercase())
}

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

/// Цвет, которым будет нарисован следующий текст, одним числом.
///
/// Вывод символа (0xc0780) берёт цвет двумя способами: при флаге
/// [gps+0x8f] — номер цвета палитры ([0x8c] цвет, [0x8e] яркость), без
/// него — RGB из [0x90..0x92]. Окна обучения и летопись красят выделенные
/// слова через RGB, поэтому [0x8c] у них всегда 7, хотя на экране слово
/// сиреневое. Здесь — тот цвет, который действительно ляжет на экран:
/// 0x01ffbb для палитры (цвет, яркость), 0x00rrggbb для RGB.
pub fn color_sig(gps: usize) -> u32 {
  if gps == 0 {
    return 0;
  }
  let byte = |off: usize| unsafe { std::ptr::read_unaligned((gps + off) as *const u8) } as u32;
  if byte(0x8f) != 0 {
    0x0100_0000 | (byte(GPS_SCREENF) << 8) | byte(GPS_SCREENBRIGHT)
  } else {
    (byte(0x90) << 16) | (byte(0x91) << 8) | byte(0x92)
  }
}

/// Перенос позиции, с которой будет нарисован следующий текст.
pub fn set_cursor(gps: usize, x: i32, y: i32) {
  if gps == 0 {
    return;
  }
  unsafe {
    std::ptr::write_unaligned((gps + GPS_SCREENX) as *mut i32, x);
    std::ptr::write_unaligned((gps + GPS_SCREENY) as *mut i32, y);
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

  // Базовый словарь загружается сразу, до чтения данных игры: если
  // загрузить его позже, часть имён успеет пройти без перевода
  let core_stamp = load_core();

  start_miss_writer();
  start_reloader(core_stamp);

  log::info!("ai: started, dictionary {PATH_AI_DICTIONARY}, misses {PATH_AI_MISSES}");
  log::info!(
    "ai: experiments: addst_flag_off={}, cstr_plus_off={}, colored_off={}, flow_off={}, english={}, screen_probe={}",
    EXPERIMENTS.addst_flag_off,
    EXPERIMENTS.cstr_plus_off,
    EXPERIMENTS.colored_off,
    EXPERIMENTS.flow_off,
    EXPERIMENTS.english,
    EXPERIMENTS.screen_probe
  );
}

/// Имя слова из словаря языков игры: одно слово из строчных латинских
/// букв, возможно с дефисами («dwarf», «maelstrom», «mountain-home»).
/// Игра ищет слова по такому имени побайтово; подмена имени переводом
/// рушит поиск, и загрузка мира падает с «Missing Word Definition».
/// Поэтому нейросетевой перевод к таким строкам не применяется.
/// Словарь сообщества dfint их переводит безопасно — его составляли
/// люди с учётом этого.
pub fn is_word_key(key: &[u8]) -> bool {
  let n = key.len();
  if n < 2 || n > 40 {
    return false;
  }
  key.iter().all(|&b| b.is_ascii_lowercase() || b == b'-')
    && key[0].is_ascii_lowercase()
    && key[n - 1].is_ascii_lowercase()
}

/// Поиск во втором (нейросетевом) словаре. Имена слов пропускаются.
pub fn lookup(key: &[u8]) -> Option<Vec<u8>> {
  // Имя слова языка — только при сборке строк. Здесь поиск для вывода на
  // экран: в режиме оригинального текста «dolomite» переводится как
  // любая надпись — на данные игры вывод не влияет
  if AI_SIZE.load(Ordering::Relaxed) == 0 || (!EXPERIMENTS.english && is_word_key(key)) {
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
  match plain_gendered(key, plain) {
    Some(value) => Some(value),
    None => {
      report_miss(key);
      None
    }
  }
}

/// Строка, выводимая на экран: сборка абзаца, перевод с контекстом,
/// запись промахов. plain — обычный поиск по словарям.
pub fn screen(gps: usize, key: &[u8], plain: fn(&[u8]) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
  note_gender(key);
  let (x, y) = cursor(gps);
  let continues = continues_paragraph(key);

  let mut run = match RUN.try_lock() {
    Ok(run) => run,
    // Состояние занято — без сборки абзаца, но и без задержки кадра
    Err(_) => return plain_or_miss(key, plain),
  };

  // Строка не на следующем ряду с того же края — прежний абзац
  // закончился на предыдущей строке
  let adjacent = !run.lines.is_empty() && x == run.x0 && y == run.last_y + 1;
  let broken = if !run.lines.is_empty() && !adjacent {
    Some(take_run(&mut run))
  } else {
    None
  };

  // Самый частый случай: обычная надпись вне абзаца
  if run.lines.is_empty() && !continues {
    drop(run);
    if let Some((lines, missed)) = broken {
      finish_run(lines, missed);
    }
    return plain_or_miss(key, plain);
  }

  // Цепочка разрослась сверх меры — это не абзац; сбрасываем её
  let overflow = if run.lines.len() >= RUN_MAX_LINES || run.bytes + key.len() > RUN_MAX_BYTES {
    Some(take_run(&mut run))
  } else {
    None
  };

  if run.lines.is_empty() {
    run.x0 = x;
  }
  run.last_y = y;
  run.lines.push(key.to_vec());
  run.bytes += key.len();

  let in_context = if AI_SIZE.load(Ordering::Relaxed) > 0 {
    lookup(&context_key(&run.lines))
  } else {
    None
  };
  let result = match in_context {
    Some(value) => Some(value),
    None => plain_gendered(key, plain),
  };
  run.missed.push(result.is_none());

  let finished = if continues { None } else { Some(take_run(&mut run)) };

  drop(run);

  if let Some((lines, missed)) = broken {
    finish_run(lines, missed);
  }
  if let Some((lines, missed)) = overflow {
    finish_lines(&lines, &missed);
  }
  if let Some((lines, missed)) = finished {
    finish_run(lines, missed);
  }

  result
}

// ==================== ЦВЕТНОЙ ТЕКСТ ====================
//
// addcoloredst рисует строку с отдельным цветом каждой буквы: так игра
// выводит мысли и характер дварфа. Строки блока стоят подряд с одного
// края (пустая строка — граница абзаца). Блок записывается в журнал
// целиком, вместе с цветами (тип Q), приложение переводит его и
// возвращает строки с метками цвета: байт 0x1C и две шестнадцатеричные
// цифры задают цвет следующих букв. Ключи словаря:
//   0x1E + строки блока до текущей через 0x1F — строка абзаца;
//   0x1D + строка — самостоятельная строка (например, мысль).

const COLORED_MAX_LINES: usize = 80;
const COLORED_MAX_BYTES: usize = 16_000;

struct ColoredLine {
  text: Vec<u8>,
  colors: Vec<u8>,
}

struct ColoredRun {
  lines: Vec<ColoredLine>,
  missed: Vec<bool>,
  x0: i32,
  last_y: i32,
}

static COLORED: LazyLock<Mutex<ColoredRun>> = LazyLock::new(|| {
  Mutex::new(ColoredRun {
    lines: Vec::new(),
    missed: Vec::new(),
    x0: 0,
    last_y: 0,
  })
});

fn colored_block_key(lines: &[ColoredLine]) -> Vec<u8> {
  let mut key = vec![0x1e];
  for (i, line) in lines.iter().enumerate() {
    if i > 0 {
      key.push(LINE_SEPARATOR);
    }
    key.extend_from_slice(&line.text);
  }
  key
}

fn hex_digit(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

/// Перевод с метками цвета → текст с нулём на конце и цвет каждой буквы.
/// Буквы до первой метки получают цвет первой буквы оригинала.
fn decode_colored(value: &[u8], original: &[u8]) -> (Vec<u8>, Vec<u8>) {
  let value = match value.last() {
    Some(0) => &value[..value.len() - 1],
    _ => value,
  };
  let mut text = Vec::with_capacity(value.len() + 1);
  let mut colors = Vec::with_capacity(value.len() + 1);
  let mut current = original.first().copied().unwrap_or(7);

  let mut i = 0;
  while i < value.len() {
    if value[i] == 0x1c && i + 2 < value.len() {
      if let (Some(h), Some(l)) = (hex_digit(value[i + 1]), hex_digit(value[i + 2])) {
        current = (h << 4) | l;
        i += 3;
        continue;
      }
    }
    text.push(value[i]);
    colors.push(current);
    i += 1;
  }
  text.push(0);
  colors.push(current);
  (text, colors)
}

fn hex_bytes(bytes: &[u8]) -> Vec<u8> {
  const DIGITS: &[u8; 16] = b"0123456789abcdef";
  let mut out = Vec::with_capacity(bytes.len() * 2);
  for &b in bytes {
    out.push(DIGITS[(b >> 4) as usize]);
    out.push(DIGITS[(b & 15) as usize]);
  }
  out
}

/// Блок закончился: если в нём есть строки без перевода, он уходит в
/// журнал целиком — строки с цветами.
fn finish_colored(lines: Vec<ColoredLine>, missed: Vec<bool>) {
  if !missed.iter().any(|&m| m) {
    return;
  }
  let mut payload = Vec::new();
  for (i, line) in lines.iter().enumerate() {
    if i > 0 {
      payload.push(LINE_SEPARATOR);
    }
    payload.extend_from_slice(&line.text);
    payload.push(0x1e);
    payload.extend_from_slice(&hex_bytes(&line.colors));
  }
  if payload.len() <= COLORED_MAX_BYTES {
    send_entry(b'Q', payload);
  }
}

/// Строка цветного текста: сборка блока, перевод, запись промахов.
/// Возвращает текст с нулём на конце и цвета букв той же длины.
pub fn colored(gps: usize, text: &[u8], colors: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
  let (x, y) = cursor(gps);
  let mut run = match COLORED.try_lock() {
    Ok(run) => run,
    Err(_) => return None,
  };

  let adjacent = !run.lines.is_empty() && x == run.x0 && y == run.last_y + 1;
  let too_long = run.lines.len() >= COLORED_MAX_LINES;
  let broken = if !run.lines.is_empty() && (!adjacent || too_long) {
    let lines = std::mem::take(&mut run.lines);
    let missed = std::mem::take(&mut run.missed);
    Some((lines, missed))
  } else {
    None
  };

  if run.lines.is_empty() {
    run.x0 = x;
  }
  run.last_y = y;
  run.lines.push(ColoredLine {
    text: text.to_vec(),
    colors: colors.to_vec(),
  });

  let value = if AI_SIZE.load(Ordering::Relaxed) > 0 {
    lookup(&colored_block_key(&run.lines)).or_else(|| {
      let mut key = Vec::with_capacity(text.len() + 1);
      key.push(0x1d);
      key.extend_from_slice(text);
      lookup(&key)
    })
  } else {
    None
  };
  let result = value.map(|v| decode_colored(&v, colors));
  run.missed.push(result.is_none() && worth_logging(text));

  drop(run);
  if let Some((lines, missed)) = broken {
    finish_colored(lines, missed);
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
pub(crate) fn send_entry(kind: u8, payload: Vec<u8>) -> bool {
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

/// Регистрация строки без перевода. Имена слов не записываются:
/// их нельзя переводить нейросетью (см. is_word_key).
pub fn report_miss(key: &[u8]) {
  if !worth_logging(key) || (!EXPERIMENTS.english && is_word_key(key)) {
    return;
  }
  // В режиме оригинального текста русская строка без латиницы — это уже
  // наш перевод (надпись, собранная игрой из словаря интерфейса)
  if EXPERIMENTS.english && !key.iter().any(|b| b.is_ascii_alphabetic()) {
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

fn start_reloader(core_stamp: Option<SystemTime>) {
  std::thread::spawn(move || {
    let mut last: Option<SystemTime> = None;
    let mut last_core = core_stamp;
    let mut last_ui: Option<SystemTime> = None;
    loop {
      if EXPERIMENTS.english {
        let ui_now = modified_at(PATH_AI_UI);
        if ui_now != last_ui {
          last_ui = load_ui();
        }
      }

      // Базовый словарь меняется редко — только когда приложение
      // замораживает в нём новые значения
      let core_now = modified_at(PATH_AI_CORE);
      if core_now != last_core {
        last_core = load_core();
      }

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
