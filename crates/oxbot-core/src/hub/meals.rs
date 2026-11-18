//! The Meal Plan, as people see and change it, and as its bot does: each
//! day's breakfast, lunch and dinner, and the grocery list. Everyone in the
//! home may change both (a person, or the home computer); every change is
//! announced so each open Meal Plan asks again.

use std::collections::HashMap;
use std::fmt::Write as _;

use oxbot_protocol::{GroceryInfo, GroceryInput, MealInfo, MealInput, ServerMsg};
use serde_json::Value;

use super::{Hub, HubError};
use crate::bot::{HOME_LEADS, Viewer};
use crate::id::{BotId, PersonId};
use crate::store::{Actor, Grocery, GroceryFields, Meal, MealFields, Slot, StoreError};
use crate::tools;

impl Hub {
    /// The dishes planned from day `from` to day `to`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn home_meals(
        &self,
        viewer: &Viewer,
        from: &str,
        to: &str,
    ) -> Result<Vec<MealInfo>, HubError> {
        self.home_actor(viewer)?;
        let names = self.person_names();
        Ok(self
            .store
            .meals_between(from, to)?
            .into_iter()
            .map(|m| meal_info(m, &names))
            .collect())
    }

    /// Plans a dish as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, what they wrote isn't valid, or the
    /// database fails.
    pub fn add_home_meal(&self, viewer: &Viewer, input: &MealInput) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        self.store.add_meal(self.meal_fields(input, None)?, &by)?;
        self.home_changed();
        Ok(())
    }

    /// Changes dish `id` as `viewer`.
    ///
    /// # Errors
    /// As [`Hub::add_home_meal`], or it's gone.
    pub fn change_home_meal(
        &self,
        viewer: &Viewer,
        id: i64,
        input: &MealInput,
    ) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        self.store
            .change_meal(id, self.meal_fields(input, None)?, &by)?;
        self.home_changed();
        Ok(())
    }

    /// Takes dish `id` off the plan, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, it's gone, or the database fails.
    pub fn remove_home_meal(&self, viewer: &Viewer, id: i64) -> Result<(), HubError> {
        self.home_actor(viewer)?;
        self.store.remove_meal(id)?;
        self.home_changed();
        Ok(())
    }

    /// The grocery list.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn home_groceries(&self, viewer: &Viewer) -> Result<Vec<GroceryInfo>, HubError> {
        self.home_actor(viewer)?;
        self.grocery_infos()
    }

    /// Adds an item to the grocery list, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, it has no name, or the database fails.
    pub fn add_home_grocery(&self, viewer: &Viewer, item: &GroceryInput) -> Result<(), HubError> {
        let by = self.home_actor(viewer)?;
        self.store.add_grocery(&grocery_fields(item, None), &by)?;
        self.home_changed();
        Ok(())
    }

    /// Ticks item `id` off (or back on), as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, it's gone, or the database fails.
    pub fn set_home_grocery(&self, viewer: &Viewer, id: i64, done: bool) -> Result<(), HubError> {
        self.home_actor(viewer)?;
        self.store.set_grocery_done(id, done)?;
        self.home_changed();
        Ok(())
    }

    /// Takes item `id` off the list, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, it's gone, or the database fails.
    pub fn remove_home_grocery(&self, viewer: &Viewer, id: i64) -> Result<(), HubError> {
        self.home_actor(viewer)?;
        self.store.remove_grocery(id)?;
        self.home_changed();
        Ok(())
    }

    /// Takes every ticked-off item off the list, as `viewer`.
    ///
    /// # Errors
    /// `viewer` isn't in the home, or the database fails.
    pub fn clear_home_groceries(&self, viewer: &Viewer) -> Result<(), HubError> {
        self.home_actor(viewer)?;
        self.store.clear_done_groceries()?;
        self.home_changed();
        Ok(())
    }

    fn home_changed(&self) {
        let _ = self.updates.send(ServerMsg::HomeChanged);
    }

    fn grocery_infos(&self) -> Result<Vec<GroceryInfo>, HubError> {
        let names = self.person_names();
        let items = self.store.groceries()?;
        let mut titles: HashMap<i64, String> = HashMap::new();
        for id in items.iter().filter_map(|g| g.fields.meal) {
            if let std::collections::hash_map::Entry::Vacant(e) = titles.entry(id)
                && let Ok(m) = self.store.meal(id)
            {
                e.insert(m.fields.title);
            }
        }
        Ok(items
            .into_iter()
            .map(|g| grocery_info(g, &names, &titles))
            .collect())
    }

    /// What a person (or the bot) wrote as a dish: people by name (one the
    /// home doesn't know is added when `added` is given, as for the bot;
    /// refused otherwise).
    fn meal_fields(
        &self,
        input: &MealInput,
        added: Option<&mut Vec<String>>,
    ) -> Result<MealFields, HubError> {
        let Some(slot) = Slot::parse(&input.slot) else {
            return Err(
                StoreError::Invalid("A meal is breakfast, lunch or dinner.".to_owned()).into(),
            );
        };
        Ok(MealFields {
            day: input.day.trim().to_owned(),
            slot,
            title: input.title.clone(),
            notes: input.notes.clone(),
            people: self.people_by_name(&input.people, added)?,
        })
    }

    fn people_by_name(
        &self,
        given: &[String],
        mut added: Option<&mut Vec<String>>,
    ) -> Result<Vec<PersonId>, HubError> {
        let names = self.person_names();
        let mut people = Vec::new();
        for name in given.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
            let known = names
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(name))
                .map(|(id, _)| id.clone());
            let id = match (known, added.as_deref_mut()) {
                (Some(id), _) => id,
                (None, Some(added)) => {
                    let person = self.person_named(name)?;
                    added.push(person.name);
                    person.id
                }
                (None, None) => {
                    return Err(StoreError::Invalid(format!(
                        "There's nobody called {name} in this home."
                    ))
                    .into());
                }
            };
            if !people.contains(&id) {
                people.push(id);
            }
        }
        Ok(people)
    }

    /// Runs one of the Meal Plan bot's tools (`tools::MEALS_TOOLS`) as `bot`.
    #[allow(clippy::too_many_lines, reason = "one arm per tool")]
    pub(super) fn meals_tool(&self, bot: &BotId, tool: &str, args: &Value) -> (bool, String) {
        let by = Actor::Bot(bot.clone());
        let text = |k: &str| tools::text_arg(args, k).trim().to_owned();
        let ids = || -> Vec<i64> {
            match args.get("ids") {
                Some(Value::Array(a)) => a.iter().filter_map(Value::as_i64).collect(),
                _ => args.get("id").and_then(Value::as_i64).into_iter().collect(),
            }
        };
        let place = if tools::GROCERY_TOOLS.contains(&tool) {
            "grocery list"
        } else {
            "Meal Plan"
        };
        let done = |r: Result<String, HubError>| match r {
            Ok(said) => {
                self.home_changed();
                (true, format!("{said} Everyone's {place} shows it now."))
            }
            Err(err) => (false, format!("{err} Nothing was changed.")),
        };
        match tool {
            tools::MEAL_PLAN_TOOL => match self.store.meals_between(&text("from"), &text("to")) {
                Ok(meals) => (true, self.plan_text(&meals)),
                Err(err) => (false, format!("Couldn't read the Meal Plan: {err}")),
            },
            tools::GROCERY_LIST_TOOL => match self.grocery_infos() {
                Ok(items) => (true, groceries_text(&items)),
                Err(err) => (false, format!("Couldn't read the grocery list: {err}")),
            },
            tools::PLAN_MEAL_TOOL => {
                let mut added = Vec::new();
                done((|| {
                    let input = meal_input(args);
                    let m = self
                        .store
                        .add_meal(self.meal_fields(&input, Some(&mut added))?, &by)?;
                    Ok(with_added(
                        format!(
                            "Planned: meal {} \"{}\", {} {}.",
                            m.id,
                            m.fields.title,
                            m.fields.day,
                            m.fields.slot.as_str()
                        ),
                        &added,
                    ))
                })())
            }
            tools::CHANGE_MEAL_TOOL => {
                let Some(id) = args.get("id").and_then(Value::as_i64) else {
                    return (false, "change_meal needs the meal's id".to_owned());
                };
                let mut added = Vec::new();
                done((|| {
                    let was = self.store.meal(id)?;
                    let mut input = meal_input(args);
                    let names = self.person_names();
                    let keep = |given: String, old: &str| {
                        if given.trim().is_empty() {
                            old.to_owned()
                        } else {
                            given
                        }
                    };
                    input.day = keep(input.day, &was.fields.day);
                    input.slot = keep(input.slot, was.fields.slot.as_str());
                    input.title = keep(input.title, &was.fields.title);
                    input.notes = keep(input.notes, &was.fields.notes);
                    if args.get("people").is_none() {
                        input.people = was
                            .fields
                            .people
                            .iter()
                            .filter_map(|p| names.get(p).cloned())
                            .collect();
                    }
                    let m = self.store.change_meal(
                        id,
                        self.meal_fields(&input, Some(&mut added))?,
                        &by,
                    )?;
                    Ok(with_added(
                        format!(
                            "Changed: meal {} \"{}\", {} {}.",
                            m.id,
                            m.fields.title,
                            m.fields.day,
                            m.fields.slot.as_str()
                        ),
                        &added,
                    ))
                })())
            }
            tools::REMOVE_MEAL_TOOL => {
                let ids = ids();
                if ids.is_empty() {
                    return (false, "remove_meal needs the meal's id".to_owned());
                }
                done((|| {
                    let mut gone = Vec::new();
                    for id in ids {
                        gone.push(format!("\"{}\"", self.store.remove_meal(id)?.fields.title));
                    }
                    Ok(format!("Took off the plan: {}.", gone.join(", ")))
                })())
            }
            tools::ADD_GROCERIES_TOOL => {
                let meal = args.get("meal").and_then(Value::as_i64);
                let items: Vec<GroceryInput> = args
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|i| GroceryInput {
                                name: tools::text_arg(i, "name").to_owned(),
                                qty: tools::text_arg(i, "qty").to_owned(),
                                aisle: tools::text_arg(i, "aisle").to_owned(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if items.is_empty() {
                    return (false, "add_groceries needs items".to_owned());
                }
                done((|| {
                    let (mut new, mut there) = (Vec::new(), Vec::new());
                    for item in &items {
                        let (g, is_new) =
                            self.store.add_grocery(&grocery_fields(item, meal), &by)?;
                        if is_new {
                            new.push(g.fields.name);
                        } else {
                            there.push(g.fields.name);
                        }
                    }
                    let mut said = format!(
                        "Added to the grocery list: {}.",
                        if new.is_empty() {
                            "nothing new".to_owned()
                        } else {
                            new.join(", ")
                        }
                    );
                    if !there.is_empty() {
                        let _ = write!(said, " Already on it: {}.", there.join(", "));
                    }
                    Ok(said)
                })())
            }
            tools::TICK_GROCERIES_TOOL => {
                let ids = ids();
                let got = args.get("done").and_then(Value::as_bool).unwrap_or(true);
                if ids.is_empty() {
                    return (false, "tick_groceries needs the items' ids".to_owned());
                }
                done((|| {
                    let mut names = Vec::new();
                    for id in ids {
                        names.push(self.store.set_grocery_done(id, got)?.fields.name);
                    }
                    Ok(format!(
                        "{}: {}.",
                        if got {
                            "Ticked off"
                        } else {
                            "Back on the list"
                        },
                        names.join(", ")
                    ))
                })())
            }
            tools::REMOVE_GROCERIES_TOOL => {
                let ids = ids();
                if ids.is_empty() {
                    return (false, "remove_groceries needs the items' ids".to_owned());
                }
                done((|| {
                    let mut names = Vec::new();
                    for id in ids {
                        names.push(self.store.remove_grocery(id)?.fields.name);
                    }
                    Ok(format!("Took off the grocery list: {}.", names.join(", ")))
                })())
            }
            _ => (false, "unknown tool".to_owned()),
        }
    }

    /// The plan as the bot reads it: who's in the home, then one line a dish.
    fn plan_text(&self, meals: &[Meal]) -> String {
        let names = self.person_names();
        let mut people: Vec<_> = names.values().cloned().collect();
        people.sort();
        let mut out = format!("People in this home: {}.\n", people.join(", "));
        if meals.is_empty() {
            out.push_str("Nothing planned then.");
        }
        for m in meals {
            let f = &m.fields;
            let _ = write!(
                out,
                "- meal {}: {} {} · {}",
                m.id,
                f.day,
                f.slot.as_str(),
                f.title
            );
            let who: Vec<&str> = f
                .people
                .iter()
                .filter_map(|p| names.get(p).map(String::as_str))
                .collect();
            if !who.is_empty() {
                let _ = write!(out, " · for {}", who.join(", "));
            }
            if !f.notes.is_empty() {
                let _ = write!(out, " · notes: {}", f.notes);
            }
            out.push('\n');
        }
        out
    }
}

fn meal_input(args: &Value) -> MealInput {
    MealInput {
        day: tools::text_arg(args, "day").to_owned(),
        slot: tools::text_arg(args, "slot").to_owned(),
        title: tools::text_arg(args, "title").to_owned(),
        notes: tools::text_arg(args, "notes").to_owned(),
        people: args
            .get("people")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn with_added(mut said: String, added: &[String]) -> String {
    if !added.is_empty() {
        let _ = write!(said, " New in this home's people: {}.", added.join(", "));
    }
    said
}

fn grocery_fields(item: &GroceryInput, meal: Option<i64>) -> GroceryFields {
    GroceryFields {
        name: item.name.clone(),
        qty: item.qty.clone(),
        aisle: item.aisle.clone(),
        meal,
    }
}

fn groceries_text(items: &[GroceryInfo]) -> String {
    if items.is_empty() {
        return "The grocery list is empty.".to_owned();
    }
    let mut out = String::new();
    for g in items {
        let i = &g.item;
        let _ = write!(
            out,
            "- item {}: {}{}",
            g.id,
            if g.done { "[got] " } else { "" },
            i.name
        );
        if !i.qty.is_empty() {
            let _ = write!(out, " · {}", i.qty);
        }
        if !i.aisle.is_empty() {
            let _ = write!(out, " · {}", i.aisle);
        }
        if !g.for_meal.is_empty() {
            let _ = write!(out, " · for {}", g.for_meal);
        }
        out.push('\n');
    }
    out
}

fn actor_name(a: Option<&Actor>, names: &HashMap<PersonId, String>) -> String {
    match a {
        Some(Actor::Person(p)) => names.get(p).cloned().unwrap_or_default(),
        Some(Actor::Bot(b)) => HOME_LEADS
            .iter()
            .find(|(id, ..)| *id == b.as_str())
            .map_or_else(|| b.to_string(), |(_, name, _)| (*name).to_owned()),
        None => String::new(),
    }
}

fn meal_info(m: Meal, names: &HashMap<PersonId, String>) -> MealInfo {
    MealInfo {
        id: m.id,
        made_by: actor_name(m.made_by.as_ref(), names),
        meal: MealInput {
            day: m.fields.day,
            slot: m.fields.slot.as_str().to_owned(),
            title: m.fields.title,
            notes: m.fields.notes,
            people: m
                .fields
                .people
                .iter()
                .filter_map(|p| names.get(p).cloned())
                .collect(),
        },
    }
}

fn grocery_info(
    g: Grocery,
    names: &HashMap<PersonId, String>,
    titles: &HashMap<i64, String>,
) -> GroceryInfo {
    GroceryInfo {
        id: g.id,
        done: g.done,
        for_meal: g
            .fields
            .meal
            .and_then(|m| titles.get(&m).cloned())
            .unwrap_or_default(),
        added_by: actor_name(g.added_by.as_ref(), names),
        item: GroceryInput {
            name: g.fields.name,
            qty: g.fields.qty,
            aisle: g.fields.aisle,
        },
    }
}
