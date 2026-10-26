//! Русский поиск в списках игры.
//!
//! Названия в памяти игры английские: перевод появляется только в момент
//! рисования. Английское «Make bed» русский запрос «кро» не найдёт никогда.
//! Каждый экран игры ищет по-своему (разбор Dwarf Fortress.exe 52.05):
//!   — задачи мастерской: запрос через simplify_string, название как есть;
//!   — запасы: название через upper_case_string, запрос заглавными из поля;
//!   — списки существ: и запрос, и название через simplify_string.
//! Общее у всех одно: запрос ищется в названии функцией стандартной
//! библиотеки std::_Traits_find (поиск подстроки). Поэтому у русского
//! поиска две части.
//!
//! 1. Перехват поиска подстроки. Если обычный поиск ничего не нашёл,
//!    запрос русский, а название английское, — хук ищет запрос в переводе
//!    названия. Адреса этой функции нет в offsets.toml dfint: хук находит
//!    её в коде игры сам, по байтам самой функции (вызовы внутри неё
//!    пропускаются). Байты не нашлись или нашлись дважды — перехват не
//!    ставится, об этом пишется в лог.
//!
//! 2. Пока в поле поиска есть русские буквы (textinput.rs), хук
//!    дописывает к английскому названию в simplify_string его перевод:
//!    «bed» → «bed␁ кровать». Это нужно, когда игра упрощает само название:
//!    из «urist mchammerer» строчными уже не найти ключ словаря «Urist
//!    McHammerer». Перевод отделён байтом 0x01 и пробелом: 0x01 меньше
//!    любой буквы, поэтому сортировка по таким строкам остаётся
//!    английской, а пробел делает перевод отдельным словом.
//!
//! Перевод берётся из всех словарей, которыми хук рисует текст:
//! нейросетевого, сообщества и базового, — в том виде, в каком игра
//! рисует надпись («Make bed»), заглавными («MAKE BED») или строчными.
//! Если названия целиком нет, переводятся отдельные слова («oak bed» →
//! «дуб», «кровать»). Название,
//! которое ни разу не попадало на экран и не переведено, найдётся только
//! по-английски.
//!
//! Запись в журнал отсюда не идёт: это не вывод на экран.
//!
//! Аварийный выключатель — строка search_ru_off в dfint-data/ai/experiments.txt.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::ai;
use crate::config::CONFIG;
use crate::dictionary;

/// Разделитель английского названия и его перевода.
pub const SEP: [u8; 2] = [0x01, b' '];
/// Длиннее этого — не название, а текст; такие строки не дополняются.
const MAX_KEY: usize = 160;
/// Предел дописываемого перевода.
const MAX_EXTRA: usize = 240;
/// Предел кэша переводов. Больше — кэш начинается заново.
const MAX_CACHE: usize = 50_000;
/// Сколько первых русских ключей записывается в лог.
const LOG_LIMIT: usize = 20;

struct Cache {
  generation: usize,
  map: HashMap<Vec<u8>, Option<Vec<u8>>>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
  Mutex::new(Cache {
    generation: 0,
    map: HashMap::new(),
  })
});
static LOGGED: AtomicUsize = AtomicUsize::new(0);

/// Русский ключ для английского названия: байт-разделитель и перевод,
/// уже упрощённый (строчные буквы) — так же, как игра упростит запрос.
/// None — ничего не дописывать.
pub fn russian_key(original: &[u8]) -> Option<Vec<u8>> {
  if ai::EXPERIMENTS.search_ru_off || !crate::textinput::russian_search_active() {
    return None;
  }
  if original.len() < 2 || original.len() > MAX_KEY {
    return None;
  }
  // Уже русское или служебное — не трогаем
  if original.iter().any(|&b| b >= 0x80 || b < 0x20) {
    return None;
  }
  if !original.iter().any(|b| b.is_ascii_alphabetic()) {
    return None;
  }

  let generation = ai::generation();
  if let Ok(mut cache) = CACHE.try_lock() {
    if cache.generation != generation {
      cache.map.clear();
      cache.generation = generation;
    }
    if let Some(value) = cache.map.get(original) {
      return value.clone();
    }
  }

  let value = build(original);

  if let Ok(mut cache) = CACHE.try_lock() {
    if cache.generation == generation {
      if cache.map.len() >= MAX_CACHE {
        cache.map.clear();
      }
      cache.map.insert(original.to_vec(), value.clone());
    }
  }

  if let Some(extra) = &value {
    if LOGGED.fetch_add(1, Ordering::Relaxed) < LOG_LIMIT {
      log::info!(
        "search: \"{}\" + {} bytes of russian key",
        String::from_utf8_lossy(original),
        extra.len()
      );
    }
  }
  value
}

fn build(original: &[u8]) -> Option<Vec<u8>> {
  let mut parts = translations(original);
  if parts.is_empty() {
    // Целиком перевода нет — по словам
    for word in original.split(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'\'')) {
      if word.len() < 2 || !word.iter().any(|b| b.is_ascii_alphabetic()) {
        continue;
      }
      for t in translations(word) {
        if !parts.contains(&t) {
          parts.push(t);
        }
      }
    }
  }
  if parts.is_empty() {
    return None;
  }

  let mut out = Vec::with_capacity(64);
  for part in parts {
    if out.len() >= MAX_EXTRA {
      break;
    }
    out.extend_from_slice(&SEP);
    for &b in &part {
      // Разметка цвета и скобки имён летописи в поиске не нужны
      if b < 0x20 || b == b'{' || b == b'}' {
        continue;
      }
      out.push(CONFIG.encoding.simplify[b as usize]);
      if out.len() >= MAX_EXTRA {
        break;
      }
    }
  }
  Some(out)
}

/// Все русские переводы строки: как есть, с заглавной и строчными
/// буквами — словари хранят надписи в том виде, в каком их нарисовала игра.
fn translations(key: &[u8]) -> Vec<Vec<u8>> {
  let mut variants: Vec<Vec<u8>> = vec![key.to_vec()];
  let mut capital = key.to_vec();
  capital[0] = capital[0].to_ascii_uppercase();
  let lower = key.to_ascii_lowercase();
  // «MAKE BED» (запасы игра сравнивает заглавными) → «Make bed»
  let mut sentence = lower.clone();
  sentence[0] = sentence[0].to_ascii_uppercase();
  for v in [capital, lower, sentence] {
    if !variants.contains(&v) {
      variants.push(v);
    }
  }

  let mut out: Vec<Vec<u8>> = Vec::new();
  for v in variants {
    for value in dictionary::search_values(&v) {
      let text = value.strip_suffix(&[0]).unwrap_or(&value).to_vec();
      // Только настоящий перевод: в нём есть русские буквы
      if text.iter().any(|&b| b >= 0x80) && !out.contains(&text) {
        out.push(text);
      }
    }
  }
  out
}


/// Есть ли русский запрос в переводе английского названия. Запрос
/// упрощается той же таблицей, что и перевод: «КРО» и «кро» одинаковы.
fn found_in_translation(hay: &[u8], needle: &[u8]) -> bool {
  let Some(key) = russian_key(hay) else {
    return false;
  };
  let needle: Vec<u8> = needle.iter().map(|&b| CONFIG.encoding.simplify[b as usize]).collect();
  !needle.is_empty() && key.windows(needle.len()).any(|w| w == needle.as_slice())
}

// ==================== ПЕРЕХВАТ ПОИСКА ПОДСТРОКИ ====================

#[cfg(target_os = "windows")]
mod find_hook {
  use retour::static_detour;
  use std::sync::atomic::{AtomicUsize, Ordering};

  /// Сколько первых русских находок записывается в лог.
  const FOUND_LOG_LIMIT: usize = 10;
  static FOUND_LOGGED: AtomicUsize = AtomicUsize::new(0);

  static_detour! {
    static FIND: unsafe extern "C" fn(*const u8, usize, usize, *const u8, usize) -> usize;
  }

  /// std::_Traits_find<char_traits<char>>(текст, длина, начало, образец,
  /// длина образца) из 52.05 (0xe7960). ?? — адреса вызовов memchr и
  /// memcmp внутри функции: они меняются от сборки к сборке.
  const PATTERN: &str = "48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 57 41 56 41 57 48 83 EC 20 \
    48 8B 7C 24 60 49 8B F1 4C 8B F9 48 3B FA 77 77 48 8B C2 48 2B C7 4C 3B C0 77 6C 48 85 FF 75 05 \
    49 8B C0 EB 69 45 0F BE 31 49 03 C8 49 8B EF 48 2B EF 48 03 EA 41 8B D6 4C 8D 45 01 4C 2B C1 \
    E8 ?? ?? ?? ?? 48 8B D8 48 85 C0 74 3B 0F 1F 40 00 0F 1F 84 00 00 00 00 00 4C 8B C7 48 8B D6 \
    48 8B CB E8 ?? ?? ?? ?? 85 C0 74 3D 48 FF C3 4C 8D 45 01 4C 2B C3 48 8B CB 41 8B D6 \
    E8 ?? ?? ?? ?? 48 8B D8 48 85 C0 75 D1 48 C7 C0 FF FF FF FF 48 8B 5C 24 40 48 8B 6C 24 48 \
    48 8B 74 24 50 48 83 C4 20 41 5F 41 5E 5F C3";

  fn parse_pattern() -> Vec<Option<u8>> {
    PATTERN
      .split_whitespace()
      .map(|t| if t == "??" { None } else { u8::from_str_radix(t, 16).ok() })
      .collect()
  }

  /// Участок кода (.text) загруженной игры: смещение от начала модуля и размер.
  unsafe fn text_section(base: usize) -> Option<(usize, usize)> {
    let rd16 = |o: usize| std::ptr::read_unaligned((base + o) as *const u16) as usize;
    let rd32 = |o: usize| std::ptr::read_unaligned((base + o) as *const u32) as usize;
    if rd16(0) != 0x5a4d {
      return None;
    }
    let nt = rd32(0x3c);
    if rd32(nt) != 0x4550 {
      return None;
    }
    let sections = rd16(nt + 6);
    let optional = rd16(nt + 20);
    let table = nt + 24 + optional;
    for i in 0..sections {
      let s = table + i * 40;
      let name = std::slice::from_raw_parts((base + s) as *const u8, 8);
      if name.starts_with(b".text") {
        return Some((rd32(s + 12), rd32(s + 8)));
      }
    }
    None
  }

  /// Адрес функции поиска подстроки: единственное совпадение образца.
  unsafe fn locate() -> Option<usize> {
    let base = *crate::utils::MODULE;
    let (rva, size) = text_section(base)?;
    let code = std::slice::from_raw_parts((base + rva) as *const u8, size);
    let pattern = parse_pattern();
    if pattern.len() < 64 || pattern.iter().take(24).any(|b| b.is_none()) {
      return None;
    }
    let head: Vec<u8> = pattern[..24].iter().map(|b| b.unwrap()).collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i + pattern.len() <= code.len() {
      if code[i] == head[0] && code[i..i + 24] == head[..] {
        let all = pattern.iter().enumerate().all(|(k, p)| p.map_or(true, |b| code[i + k] == b));
        if all {
          found.push(base + rva + i);
          if found.len() > 1 {
            break;
          }
        }
      }
      i += 1;
    }
    match found.as_slice() {
      [one] => Some(*one),
      [] => {
        log::warn!("search: substring search function not found, russian search only through simplify_string");
        None
      }
      _ => {
        log::warn!("search: substring search function found twice, hook not installed");
        None
      }
    }
  }

  fn detour(hay: *const u8, hay_size: usize, start: usize, needle: *const u8, needle_size: usize) -> usize {
    let result = unsafe { FIND.call(hay, hay_size, start, needle, needle_size) };
    if result != usize::MAX
      || start != 0
      || needle_size == 0
      || hay_size < 2
      || hay.is_null()
      || needle.is_null()
      || !crate::textinput::russian_search_active()
    {
      return result;
    }
    let (hay_s, needle_s) = unsafe {
      (
        std::slice::from_raw_parts(hay, hay_size),
        std::slice::from_raw_parts(needle, needle_size),
      )
    };
    // Только русский запрос в английском названии
    if !needle_s.iter().any(|&b| b >= 0x80) {
      return result;
    }
    if super::found_in_translation(hay_s, needle_s) {
      if FOUND_LOGGED.fetch_add(1, Ordering::Relaxed) < FOUND_LOG_LIMIT {
        log::info!(
          "search: found russian query ({} bytes) in translation of \"{}\"",
          needle_size,
          String::from_utf8_lossy(hay_s)
        );
      }
      0
    } else {
      result
    }
  }

  pub unsafe fn attach() {
    let Some(address) = locate() else {
      return;
    };
    let target: unsafe extern "C" fn(*const u8, usize, usize, *const u8, usize) -> usize = std::mem::transmute(address);
    match FIND.initialize(target, detour).and_then(|d| d.enable()) {
      Ok(_) => log::info!(
        "search: substring search hooked at 0x{:x}",
        address - *crate::utils::MODULE
      ),
      Err(err) => log::error!("search: unable to hook substring search: {err:?}"),
    }
  }

  pub unsafe fn set_enabled(on: bool) {
    if FIND.is_enabled() != on {
      let _ = if on { FIND.enable() } else { FIND.disable() };
    }
  }
}

/// Перехват поиска подстроки (часть 1). Вызывается вместе с остальными
/// перехватами поиска.
pub unsafe fn attach() {
  if ai::EXPERIMENTS.search_ru_off {
    return;
  }
  #[cfg(target_os = "windows")]
  find_hook::attach();
}

/// Включение и выключение вместе с остальными перехватами (Ctrl+F2).
#[allow(dead_code)]
pub unsafe fn set_enabled(on: bool) {
  #[cfg(target_os = "windows")]
  find_hook::set_enabled(on);
  #[cfg(not(target_os = "windows"))]
  let _ = on;
}
