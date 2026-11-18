//! The Meal Plan (migration 32): what's planned for each day's breakfast,
//! lunch and dinner (a meal may have several dishes), and the family's
//! grocery list. Everyone in the home sees and changes both.

use rusqlite::{OptionalExtension as _, Row, params};

use super::{Actor, Store, StoreError, is_day, now_ms};
use crate::id::PersonId;

/// A meal of the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// Breakfast.
    Breakfast,
    /// Lunch.
    Lunch,
    /// Dinner.
    Dinner,
}

impl Slot {
    /// As stored and sent: `breakfast`, `lunch` or `dinner`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Breakfast => "breakfast",
            Self::Lunch => "lunch",
            Self::Dinner => "dinner",
        }
    }

    /// From [`Slot::as_str`] (any case); `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "breakfast" => Self::Breakfast,
            "lunch" => Self::Lunch,
            "dinner" | "supper" => Self::Dinner,
            _ => return None,
        })
    }
}

/// What a planned dish says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MealFields {
    /// Its day, `2026-11-17`.
    pub day: String,
    /// Which meal.
    pub slot: Slot,
    /// What it is ("Chicken tacos").
    pub title: String,
    /// More: a recipe link, what to prep, who's cooking.
    pub notes: String,
    /// Who it's for; none: everyone.
    pub people: Vec<PersonId>,
}

impl MealFields {
    fn checked(mut self) -> Result<Self, StoreError> {
        self.title = self.title.split_whitespace().collect::<Vec<_>>().join(" ");
        self.notes = self.notes.trim().to_owned();
        if self.title.is_empty() {
            return Err(StoreError::Invalid("A meal needs a name.".to_owned()));
        }
        if !is_day(&self.day) {
            return Err(StoreError::Invalid(
                "A meal's day is written 2026-11-17.".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// A planned dish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meal {
    /// Its id.
    pub id: i64,
    /// What it says.
    pub fields: MealFields,
    /// Who planned it.
    pub made_by: Option<Actor>,
    /// Who changed it last.
    pub changed_by: Option<Actor>,
}

/// What a grocery item says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroceryFields {
    /// What to buy ("Milk").
    pub name: String,
    /// How much ("2 L"), if said.
    pub qty: String,
    /// Where in the store ("Dairy"), to group the list.
    pub aisle: String,
    /// The meal it's for, if any.
    pub meal: Option<i64>,
}

/// An item on the grocery list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grocery {
    /// Its id.
    pub id: i64,
    /// What it says.
    pub fields: GroceryFields,
    /// Got (ticked off).
    pub done: bool,
    /// Who added it.
    pub added_by: Option<Actor>,
}

const MEAL_COLUMNS: &str = "id, day, slot, title, notes, people, made_by, changed_by";
const GROCERY_COLUMNS: &str = "id, name, qty, aisle, meal, done, added_by";

fn meal_row(r: &Row<'_>) -> rusqlite::Result<Meal> {
    let people: String = r.get(5)?;
    Ok(Meal {
        id: r.get(0)?,
        fields: MealFields {
            day: r.get(1)?,
            slot: Slot::parse(&r.get::<_, String>(2)?).unwrap_or(Slot::Dinner),
            title: r.get(3)?,
            notes: r.get(4)?,
            people: serde_json::from_str::<Vec<String>>(&people)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|p| PersonId::new(p).ok())
                .collect(),
        },
        made_by: Actor::from_text(&r.get::<_, String>(6)?),
        changed_by: Actor::from_text(&r.get::<_, String>(7)?),
    })
}

fn grocery_row(r: &Row<'_>) -> rusqlite::Result<Grocery> {
    Ok(Grocery {
        id: r.get(0)?,
        fields: GroceryFields {
            name: r.get(1)?,
            qty: r.get(2)?,
            aisle: r.get(3)?,
            meal: r.get(4)?,
        },
        done: r.get(5)?,
        added_by: Actor::from_text(&r.get::<_, String>(6)?),
    })
}

fn people_json(people: &[PersonId]) -> String {
    let ids: Vec<&str> = people.iter().map(PersonId::as_str).collect();
    serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_owned())
}

fn one_line(t: &str) -> String {
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Store {
    /// Plans a dish.
    ///
    /// # Errors
    /// No name, a day that isn't one, or SQLite fails.
    pub fn add_meal(&self, fields: MealFields, by: &Actor) -> Result<Meal, StoreError> {
        let f = fields.checked()?;
        let conn = self.conn();
        conn.execute(
            "INSERT INTO home_meals (day, slot, title, notes, people, made_by, changed_by, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            params![f.day, f.slot.as_str(), f.title, f.notes, people_json(&f.people), by.to_text(), now_ms()],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.meal(id)
    }

    /// Changes dish `id`.
    ///
    /// # Errors
    /// As [`Store::add_meal`], or it isn't there.
    pub fn change_meal(&self, id: i64, fields: MealFields, by: &Actor) -> Result<Meal, StoreError> {
        let f = fields.checked()?;
        let n = self.conn().execute(
            "UPDATE home_meals SET day = ?2, slot = ?3, title = ?4, notes = ?5, people = ?6,
               changed_by = ?7, updated_ms = ?8 WHERE id = ?1",
            params![
                id,
                f.day,
                f.slot.as_str(),
                f.title,
                f.notes,
                people_json(&f.people),
                by.to_text(),
                now_ms()
            ],
        )?;
        if n == 0 {
            return Err(StoreError::Invalid(format!("There's no meal {id}.")));
        }
        self.meal(id)
    }

    /// Takes dish `id` off the plan (its groceries stay, no longer tied to
    /// it). Returns what it was.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn remove_meal(&self, id: i64) -> Result<Meal, StoreError> {
        let meal = self.meal(id)?;
        let conn = self.conn();
        conn.execute(
            "UPDATE home_groceries SET meal = NULL WHERE meal = ?1",
            [id],
        )?;
        conn.execute("DELETE FROM home_meals WHERE id = ?1", [id])?;
        Ok(meal)
    }

    /// Dish `id`.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn meal(&self, id: i64) -> Result<Meal, StoreError> {
        self.conn()
            .query_row(
                &format!("SELECT {MEAL_COLUMNS} FROM home_meals WHERE id = ?1"),
                [id],
                meal_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid(format!("There's no meal {id}.")))
    }

    /// The dishes planned from day `from` to day `to` (both included), by
    /// day, then breakfast, lunch, dinner, then as added.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn meals_between(&self, from: &str, to: &str) -> Result<Vec<Meal>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEAL_COLUMNS} FROM home_meals WHERE day >= ?1 AND day <= ?2
             ORDER BY day, CASE slot WHEN 'breakfast' THEN 0 WHEN 'lunch' THEN 1 ELSE 2 END, id"
        ))?;
        let rows = stmt.query_map(params![from, to], meal_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Adds an item to the grocery list; one already there and not got
    /// (same name, any case) is kept instead, with this amount if it had
    /// none. Returns it and whether it's new.
    ///
    /// # Errors
    /// No name, or SQLite fails.
    pub fn add_grocery(
        &self,
        fields: &GroceryFields,
        by: &Actor,
    ) -> Result<(Grocery, bool), StoreError> {
        let name = one_line(&fields.name);
        if name.is_empty() {
            return Err(StoreError::Invalid(
                "A grocery item needs a name.".to_owned(),
            ));
        }
        let (qty, aisle) = (one_line(&fields.qty), one_line(&fields.aisle));
        let conn = self.conn();
        let there: Option<i64> = conn
            .query_row(
                "SELECT id FROM home_groceries WHERE done = 0 AND lower(name) = lower(?1)",
                [&name],
                |r| r.get(0),
            )
            .optional()?;
        let (id, new) = if let Some(id) = there {
            conn.execute(
                "UPDATE home_groceries SET qty = CASE qty WHEN '' THEN ?2 ELSE qty END,
                   aisle = CASE aisle WHEN '' THEN ?3 ELSE aisle END, updated_ms = ?4 WHERE id = ?1",
                params![id, qty, aisle, now_ms()],
            )?;
            (id, false)
        } else {
            conn.execute(
                "INSERT INTO home_groceries (name, qty, aisle, meal, added_by, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![name, qty, aisle, fields.meal, by.to_text(), now_ms()],
            )?;
            (conn.last_insert_rowid(), true)
        };
        drop(conn);
        Ok((self.grocery(id)?, new))
    }

    /// Ticks item `id` off (or back on).
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn set_grocery_done(&self, id: i64, done: bool) -> Result<Grocery, StoreError> {
        let n = self.conn().execute(
            "UPDATE home_groceries SET done = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id, done, now_ms()],
        )?;
        if n == 0 {
            return Err(StoreError::Invalid(format!(
                "There's no grocery item {id}."
            )));
        }
        self.grocery(id)
    }

    /// Takes item `id` off the list. Returns what it was.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn remove_grocery(&self, id: i64) -> Result<Grocery, StoreError> {
        let item = self.grocery(id)?;
        self.conn()
            .execute("DELETE FROM home_groceries WHERE id = ?1", [id])?;
        Ok(item)
    }

    /// Takes every ticked-off item off the list. Returns how many.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn clear_done_groceries(&self) -> Result<usize, StoreError> {
        Ok(self
            .conn()
            .execute("DELETE FROM home_groceries WHERE done = 1", [])?)
    }

    /// Item `id`.
    ///
    /// # Errors
    /// It isn't there, or SQLite fails.
    pub fn grocery(&self, id: i64) -> Result<Grocery, StoreError> {
        self.conn()
            .query_row(
                &format!("SELECT {GROCERY_COLUMNS} FROM home_groceries WHERE id = ?1"),
                [id],
                grocery_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::Invalid(format!("There's no grocery item {id}.")))
    }

    /// The grocery list: what's still to get first, then by aisle and name.
    ///
    /// # Errors
    /// SQLite fails.
    pub fn groceries(&self) -> Result<Vec<Grocery>, StoreError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {GROCERY_COLUMNS} FROM home_groceries
             ORDER BY done, CASE aisle WHEN '' THEN 1 ELSE 0 END, lower(aisle), lower(name), id"
        ))?;
        let rows = stmt.query_map([], grocery_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tacos(day: &str) -> MealFields {
        MealFields {
            day: day.into(),
            slot: Slot::Dinner,
            title: " Chicken   tacos ".into(),
            notes: String::new(),
            people: vec![],
        }
    }

    #[test]
    fn meals_are_planned_by_day_and_meal() {
        let store = Store::open_in_memory().unwrap();
        let by = Actor::Person(PersonId::random());
        let dinner = store.add_meal(tacos("2026-11-18"), &by).unwrap();
        assert_eq!(dinner.fields.title, "Chicken tacos");
        store
            .add_meal(
                MealFields {
                    slot: Slot::Breakfast,
                    title: "Oatmeal".into(),
                    ..tacos("2026-11-18")
                },
                &by,
            )
            .unwrap();
        store.add_meal(tacos("2026-11-25"), &by).unwrap();
        let week = store.meals_between("2026-11-16", "2026-11-22").unwrap();
        assert_eq!(
            week.iter()
                .map(|m| m.fields.title.as_str())
                .collect::<Vec<_>>(),
            ["Oatmeal", "Chicken tacos"]
        );
        let moved = store
            .change_meal(
                dinner.id,
                MealFields {
                    day: "2026-11-19".into(),
                    ..dinner.fields.clone()
                },
                &by,
            )
            .unwrap();
        assert_eq!(moved.fields.day, "2026-11-19");
        assert!(
            store
                .add_meal(
                    MealFields {
                        title: " ".into(),
                        ..tacos("2026-11-18")
                    },
                    &by
                )
                .is_err()
        );
        assert!(store.add_meal(tacos("Tuesday"), &by).is_err());
        assert_eq!(Slot::parse("Supper"), Some(Slot::Dinner));
    }

    #[test]
    fn the_grocery_list_keeps_one_of_each_until_cleared() {
        let store = Store::open_in_memory().unwrap();
        let by = Actor::Person(PersonId::random());
        let meal = store.add_meal(tacos("2026-11-18"), &by).unwrap();
        let item = |name: &str, qty: &str, aisle: &str| GroceryFields {
            name: name.into(),
            qty: qty.into(),
            aisle: aisle.into(),
            meal: Some(meal.id),
        };
        let (milk, new) = store.add_grocery(&item("Milk", "", "Dairy"), &by).unwrap();
        assert!(new);
        let (again, new) = store.add_grocery(&item("milk", "2 L", ""), &by).unwrap();
        assert!(!new, "one milk");
        assert_eq!(
            (
                again.id,
                again.fields.qty.as_str(),
                again.fields.aisle.as_str()
            ),
            (milk.id, "2 L", "Dairy")
        );
        store.add_grocery(&item("Tortillas", "", ""), &by).unwrap();
        store.set_grocery_done(milk.id, true).unwrap();
        let list = store.groceries().unwrap();
        assert_eq!(
            list.iter()
                .map(|g| g.fields.name.as_str())
                .collect::<Vec<_>>(),
            ["Tortillas", "Milk"]
        );
        // Got milk, then milk again: a new item.
        assert!(store.add_grocery(&item("Milk", "", ""), &by).unwrap().1);
        assert_eq!(store.clear_done_groceries().unwrap(), 1);
        // Taking the meal off keeps its groceries.
        store.remove_meal(meal.id).unwrap();
        assert!(
            store
                .groceries()
                .unwrap()
                .iter()
                .all(|g| g.fields.meal.is_none())
        );
    }
}
