use anyhow::Result;
use std::collections::HashMap;
use std::io::prelude::*;

use crate::ai;
use crate::constants::PATH_DICTIONARY;
use crate::utils;

#[static_init::dynamic]
pub static DICTIONARY: Dictionary = Dictionary::new(PATH_DICTIONARY);

#[allow(dead_code)]
pub struct Dictionary {
  map: HashMap<Vec<u8>, Vec<u8>>,
  path: &'static str,
}

impl Dictionary {
  pub fn new(path: &'static str) -> Self {
    Self {
      map: match Dictionary::load(path) {
        Ok(value) => value,
        Err(_) => {
          log::error!("unable to load dictionary {path}");
          utils::message_box(
            "dfint hook error",
            format!("Unable to load dictionary {path}").as_str(),
            utils::MessageIconType::Warning,
          );
          HashMap::<Vec<u8>, Vec<u8>>::new()
        }
      },
      path,
    }
  }

  pub fn get(&self, key: &[u8]) -> Option<&Vec<u8>> {
    self.map.get(key)
  }

  pub fn size(&self) -> usize {
    self.map.len()
  }

  pub fn _data(&self) -> &HashMap<Vec<u8>, Vec<u8>> {
    &self.map
  }

  pub fn _reload(&mut self) -> Result<()> {
    self.map = Self::load(self.path)?;
    Ok(())
  }

  /// Разбор CSV-словаря. Открыт для модуля ai: второй словарь
  /// читается тем же разбором, чтобы формат файлов был одинаковым.
  #[allow(unused_must_use)]
  pub fn load(path: &str) -> Result<HashMap<Vec<u8>, Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut contents: Vec<u8> = Vec::new();
    file.read_to_end(&mut contents);
    let mut map = HashMap::<Vec<u8>, Vec<u8>>::new();
    const QUOTE: &u8 = &b"\""[0];
    for item in regex::bytes::Regex::new(r#"(?-u)"(.+)","(.+)""#)?.captures_iter(&contents) {
      let mut k = item[1].to_vec();
      let mut v = item[2].to_vec();
      v.push(0);
      k.dedup_by(|a, b| a == QUOTE && b == QUOTE);
      v.dedup_by(|a, b| a == QUOTE && b == QUOTE);
      map.insert(k, v);
    }
    Ok(map)
  }
}

/// Словарь сообщества в работе: везде, кроме режима оригинального текста.
fn community(key: &[u8]) -> Option<Vec<u8>> {
  if ai::EXPERIMENTS.english {
    return None;
  }
  DICTIONARY.get(key).cloned()
}

/// Поиск там, где игра копирует и склеивает строки: словарь сообщества,
/// затем базовый (замороженный) нейросетевой словарь. Новые переводы
/// нейросети здесь не применяются: строка может быть не только текстом
/// на экране, но и именем, которое игра запишет в сохранение.
/// В режиме оригинального текста словарь сообщества пропускается — игра
/// собирает строки по-английски, и до экрана доходит настоящий текст.
pub fn lookup(key: &[u8]) -> Option<Vec<u8>> {
  if let Some(value) = community(key) {
    return Some(value);
  }
  // Режим оригинального текста: надписи интерфейса из нашего словаря
  // подставляются при сборке строк, как раньше словарь сообщества
  if ai::EXPERIMENTS.english {
    if let Some(value) = ai::lookup_ui(key) {
      return Some(value);
    }
  }
  ai::lookup_core(key)
}

/// Поиск для вывода на экран: словарь сообщества, нейросетевой, затем
/// базовый. На экране перевод ничего в логике игры не меняет, поэтому
/// здесь действуют и новые переводы, и ручные правки.
pub fn lookup_all(key: &[u8]) -> Option<Vec<u8>> {
  if let Some(value) = community(key) {
    return Some(value);
  }
  if let Some(value) = ai::lookup(key) {
    return Some(value);
  }
  ai::lookup_core(key)
}

/// Поиск для строк, выводимых на экран. Кроме обычного поиска здесь
/// собираются абзацы из строк, на которые игра режет длинный текст,
/// и подставляется перевод строки с учётом её места в абзаце.
/// Строка без перевода уходит в журнал для переводчика.
pub fn lookup_screen(gps: usize, key: &[u8]) -> Option<Vec<u8>> {
  ai::screen(gps, key, lookup_all)
}
