//! Выравнивание надписей, которые игра ставит по центру или по правому краю.
//!
//! Игра сама считает, с какого столбца рисовать надпись, — по её длине.
//! Кнопки главного меню, например (Dwarf_Fortress.exe 52.05, 0x255cc1):
//!
//!   буфер ← "Create new world"           (литерал копируется в char[])
//!   x = (левый + правый) / 2 − (strlen(буфер) − 1) / 2
//!   locate(y, x); addst(std::string(буфер))
//!
//! Длина считается по английскому тексту ещё до того, как из буфера
//! собирается строка, поэтому ни словарь сообщества, ни наш перевод при
//! сборке строк на этот расчёт не влияют: русская надпись начинается там,
//! где начиналась бы английская, и съезжает вправо.
//!
//! Места, где игра так считает столбец, хук находит сам разбором кода
//! игры (align_scan.rs) при первом запуске каждой версии и кладёт таблицу
//! в dfint-data/ai/align_<метка>.txt; метка — отметка времени из
//! заголовка exe, как у таблицы смещений dfint. Обновилась игра — таблица
//! строится заново. Для 52.05 есть и встроенная таблица, проверенная
//! вручную: на случай, если разбор не удался.
//!
//! Строка таблицы: «вид адрес вычитание источник»:
//!   вид       — c (центр) или r (правый край);
//!   адрес     — адрес возврата вызова (RVA, hex);
//!   вычитание — 1, если игра берёт (длина − 1) / 2, иначе длина / 2;
//!   источник  — l: длина английского литерала (strlen буфера или число,
//!               посчитанное компилятором); s: длина самой строки.
//!
//! Чей вызов, хук узнаёт по стеку: RtlCaptureStackBackTrace раскручивает
//! кадры по таблицам раскрутки (.pdata), первый адрес в коде игры, перед
//! которым стоит call на перехваченную функцию, — место вызова.
//!
//! Сдвиг: надпись переведена при выводе — игра считала столбец по
//! английской длине при любом источнике. Надпись собрана игрой из
//! словаря интерфейса (уже русская) — по английской длине игра считала
//! только для литерала (l), а длина строки (s) уже русская.
//!
//! Аварийный выключатель — строка align_off в experiments.txt.

use std::collections::HashMap;
use std::sync::{LazyLock, OnceLock};

use crate::ai;
use crate::config::CONFIG;
use crate::constants::{PATH_AI_DIR, PATH_EXE};
use crate::utils;

#[derive(Clone, Copy)]
enum Kind {
  Center { dec: bool },
  Right,
}

#[derive(Clone, Copy)]
struct Site {
  kind: Kind,
  literal: bool,
}

/// Встроенная таблица для версии, на которой разбор проверен вручную.
/// Нужна, если разобрать exe не получилось.
const TABLES: &[(u32, &str)] = &[(0x68d64ce7, include_str!("align_68d64ce7.txt"))];

/// Версия формата кеша: меняется, когда меняется сам разбор.
const CACHE_VERSION: &str = "align-scan v1";

static SITES: OnceLock<HashMap<u32, Site>> = OnceLock::new();

fn parse(text: &str) -> HashMap<u32, Site> {
  let mut map = HashMap::new();
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 {
      continue;
    }
    let Ok(rva) = u32::from_str_radix(parts[1], 16) else {
      continue;
    };
    let kind = match parts[0] {
      "c" => Kind::Center { dec: parts[2] == "1" },
      "r" => Kind::Right,
      _ => continue,
    };
    map.insert(rva, Site { kind, literal: parts[3] == "l" });
  }
  map
}

/// Таблица для текущей версии игры: из кеша, разбором exe или встроенная.
fn load() -> HashMap<u32, Site> {
  let stamp = CONFIG.offset_metadata.checksum;
  let cache = format!("{PATH_AI_DIR}/align_{stamp:x}.txt");

  if let Ok(text) = std::fs::read_to_string(&cache) {
    if text.lines().next().is_some_and(|l| l.contains(CACHE_VERSION)) {
      let map = parse(&text);
      if !map.is_empty() {
        log::info!("align: {} sites from {cache}", map.len());
        return map;
      }
    }
  }

  // Разбор кода игры: при первом запуске каждой версии
  let targets: Vec<u32> = CONFIG
    .offset
    .as_ref()
    .map(|o| [o.addst, o.addst_top].into_iter().flatten().map(|v| v as u32).collect())
    .unwrap_or_default();
  if !targets.is_empty() {
    let started = std::time::Instant::now();
    match std::fs::read(PATH_EXE).ok().and_then(|data| crate::align_scan::scan(&data, &targets)) {
      Some(sites) if !sites.is_empty() => {
        let text = format!("# {CACHE_VERSION}\n{}", crate::align_scan::format(stamp, &sites));
        let map = parse(&text);
        log::info!("align: {} sites found in game code in {:?}", map.len(), started.elapsed());
        if let Some((_, builtin)) = TABLES.iter().find(|(s, _)| *s == stamp) {
          let same = parse(builtin).keys().collect::<std::collections::HashSet<_>>()
            == map.keys().collect::<std::collections::HashSet<_>>();
          log::info!("align: built-in table for this version {}", if same { "matches" } else { "differs" });
        }
        if let Err(err) = std::fs::write(&cache, text) {
          log::warn!("align: unable to write {cache}: {err}");
        }
        return map;
      }
      _ => log::warn!("align: game code scan failed"),
    }
  }

  match TABLES.iter().find(|(s, _)| *s == stamp) {
    Some((_, text)) => {
      let map = parse(text);
      log::info!("align: {} sites from built-in table", map.len());
      map
    }
    None => {
      log::info!("align: no table for 0x{stamp:x}");
      HashMap::new()
    }
  }
}

/// Таблица строится в отдельном потоке: разбор exe занимает доли секунды,
/// но запуск игры ждать его не должен. До готовности выравнивания нет.
pub fn start() {
  std::thread::spawn(|| {
    let _ = SITES.set(load());
  });
}

/// Столбец, с которого игра начала бы надпись длиной len, относительно
/// её «опорной» точки (центра или правого края).
fn offset(kind: Kind, len: i32) -> i32 {
  match kind {
    Kind::Center { dec: true } => (len - 1).max(0) >> 1,
    Kind::Center { dec: false } => len >> 1,
    Kind::Right => len,
  }
}

#[cfg(target_os = "windows")]
fn caller(target: usize) -> Option<u32> {
  #[link(name = "kernel32")]
  extern "system" {
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, trace: *mut *mut std::ffi::c_void, hash: *mut u32) -> u16;
  }
  let module = utils::address(0);
  let mut frames = [std::ptr::null_mut(); 12];
  let n = unsafe { RtlCaptureStackBackTrace(0, frames.len() as u32, frames.as_mut_ptr(), std::ptr::null_mut()) };
  for &frame in frames.iter().take(n as usize) {
    let ret = frame as usize;
    if ret < module + 0x1000 + 5 || ret > module + 0x0400_0000 {
      continue;
    }
    // Перед адресом возврата — call rel32 на перехваченную функцию
    let call = unsafe { std::slice::from_raw_parts((ret - 5) as *const u8, 5) };
    if call[0] != 0xe8 {
      continue;
    }
    let rel = i32::from_le_bytes([call[1], call[2], call[3], call[4]]) as isize;
    if (ret as isize + rel) as usize == target {
      return Some((ret - module) as u32);
    }
  }
  None
}

#[cfg(target_os = "linux")]
fn caller(_target: usize) -> Option<u32> {
  None
}

/// Сдвиг столбца для надписи. target — адрес перехваченной функции
/// (addst или addst_top). en_len — длина английской надписи, ru_len —
/// русской. constructed — русскую надпись собрала сама игра (словарь
/// интерфейса), а не перевод при выводе.
pub fn shift(target: usize, en_len: usize, ru_len: usize, constructed: bool) -> i32 {
  if ai::EXPERIMENTS.align_off || en_len == ru_len || target == 0 {
    return 0;
  }
  let Some(sites) = SITES.get() else {
    return 0;
  };
  if sites.is_empty() {
    return 0;
  }
  let Some(rva) = caller(target) else {
    return 0;
  };
  let Some(site) = sites.get(&rva) else {
    return 0;
  };
  // Игра считала по длине уже русской строки — сдвигать нечего
  if constructed && !site.literal {
    return 0;
  }
  offset(site.kind, en_len as i32) - offset(site.kind, ru_len as i32)
}

/// Адреса перехваченных функций вывода — по таблице смещений dfint.
pub static ADDST: LazyLock<usize> =
  LazyLock::new(|| CONFIG.offset.as_ref().and_then(|o| o.addst).map(utils::address).unwrap_or(0));
pub static ADDST_TOP: LazyLock<usize> =
  LazyLock::new(|| CONFIG.offset.as_ref().and_then(|o| o.addst_top).map(utils::address).unwrap_or(0));

/// Сдвиг для надписи перед выводом: переведённой при выводе (translated —
/// значение словаря с нулём на конце) или собранной игрой из словаря
/// интерфейса (тогда английская длина берётся из этого словаря).
pub fn for_draw(target: usize, key: &[u8], translated: Option<&Vec<u8>>) -> i32 {
  match translated {
    // Надпись сообщества уже подогнана под место его авторами
    Some(_) if crate::dictionary::from_community(key) => 0,
    Some(t) => shift(target, key.len(), t.len().saturating_sub(1), false),
    None => match ai::ui_source_len(key) {
      Some(en) => shift(target, en, key.len(), true),
      None => 0,
    },
  }
}
