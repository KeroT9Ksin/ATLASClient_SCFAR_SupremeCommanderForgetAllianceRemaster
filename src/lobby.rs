use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Balance { Faf, Atlas, Gap }

impl Balance {
    pub fn all() -> [Balance; 3] { [Balance::Faf, Balance::Atlas, Balance::Gap] }
    pub fn label(&self) -> &'static str {
        match self { Balance::Faf => "FAF", Balance::Atlas => "ATLAS", Balance::Gap => "GAP" }
    }
    pub fn desc(&self) -> &'static str {
        match self {
            Balance::Faf => "Стандартный баланс FAForever",
            Balance::Atlas => "Собственный баланс ATLAS",
            Balance::Gap => "Global Armor Pack",
        }
    }
}

pub struct Lobby {
    pub id: u32, pub balance: Balance, pub map: String, pub has_password: bool,
    pub players: Vec<String>, pub chat: Vec<(String, String)>, pub created_at: Instant,
}

impl Lobby {
    pub fn new(balance: Balance, map: String, has_password: bool) -> Self {
        let id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs()).unwrap_or(0) % 9000 + 1000) as u32;
        Lobby {
            id, balance, map, has_password,
            players: vec!["Vlad_Atlas".into()],
            chat: vec![("СИСТЕМА".into(),
                format!("Лобби #{} создано. Баланс: {}. Ожидание игроков...", id, balance.label()))],
            created_at: Instant::now(),
        }
    }
}