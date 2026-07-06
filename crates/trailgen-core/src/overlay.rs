use crate::difficulty::DifficultyWeights;
use crate::geo::{Coord, LineString};
use crate::model::{
    Access, CrossingEvidence, CrossingKind, Edge, EdgeTravel, Provenance, Terrain, TerrainEvidence,
    TrailGraph,
};
use crate::{Result, TrailgenError};
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PlanningDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl PlanningDate {
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if year >= 1 && month >= 1 && month <= 12 && day >= 1 && day <= days_in_month(year, month) {
            Some(Self { year, month, day })
        } else {
            None
        }
    }

    #[must_use]
    pub const fn weekday(self) -> Weekday {
        const OFFSETS: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let month = self.month as usize;
        let year = if month < 3 {
            self.year as i32 - 1
        } else {
            self.year as i32
        };
        match (year + year / 4 - year / 100 + year / 400 + OFFSETS[month - 1] + self.day as i32)
            .rem_euclid(7)
        {
            0 => Weekday::Sunday,
            1 => Weekday::Monday,
            2 => Weekday::Tuesday,
            3 => Weekday::Wednesday,
            4 => Weekday::Thursday,
            5 => Weekday::Friday,
            6 => Weekday::Saturday,
            _ => unreachable!(),
        }
    }
}

impl Display for PlanningDate {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for PlanningDate {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = raw.trim().split('-');
        let year = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u16>()
            .map_err(|error| error.to_string())?;
        let month = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        let day = parts
            .next()
            .ok_or_else(|| "date must be YYYY-MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        if parts.next().is_some() {
            return Err("date must be YYYY-MM-DD".to_owned());
        }
        Self::new(year, month, day).ok_or_else(|| "invalid civil date".to_owned())
    }
}

impl Serialize for PlanningDate {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanningDate {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct DateVisitor;

        impl Visitor<'_> for DateVisitor {
            type Value = PlanningDate;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("a YYYY-MM-DD civil date")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<PlanningDate>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(DateVisitor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PlanningTime {
    pub hour: u8,
    pub minute: u8,
}

impl PlanningTime {
    #[must_use]
    pub const fn new(hour: u8, minute: u8) -> Option<Self> {
        if hour < 24 && minute < 60 {
            Some(Self { hour, minute })
        } else {
            None
        }
    }

    const fn minute_of_day(self) -> u16 {
        self.hour as u16 * 60 + self.minute as u16
    }
}

impl Display for PlanningTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}:{:02}", self.hour, self.minute)
    }
}

impl FromStr for PlanningTime {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = raw.trim().split(':');
        let hour = parts
            .next()
            .ok_or_else(|| "time must be HH:MM".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        let minute = parts
            .next()
            .ok_or_else(|| "time must be HH:MM".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        match parts.next() {
            None => {}
            Some("00") if parts.next().is_none() => {}
            _ => return Err("time must be HH:MM or HH:MM:00".to_owned()),
        }
        Self::new(hour, minute).ok_or_else(|| "invalid civil time".to_owned())
    }
}

impl Serialize for PlanningTime {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for PlanningTime {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TimeVisitor;

        impl Visitor<'_> for TimeVisitor {
            type Value = PlanningTime;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("an HH:MM civil time")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<PlanningTime>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(TimeVisitor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DailyTimeWindow {
    pub from: PlanningTime,
    pub to: PlanningTime,
}

impl DailyTimeWindow {
    #[must_use]
    pub const fn new(from: PlanningTime, to: PlanningTime) -> Self {
        Self { from, to }
    }

    #[must_use]
    pub const fn contains(self, time: PlanningTime) -> bool {
        let from = self.from.minute_of_day();
        let to = self.to.minute_of_day();
        let time = time.minute_of_day();
        if from <= to {
            from <= time && time <= to
        } else {
            from <= time || time <= to
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlanningMoment {
    pub date: Option<PlanningDate>,
    pub time: Option<PlanningTime>,
}

impl PlanningMoment {
    #[must_use]
    pub const fn new(date: Option<PlanningDate>, time: Option<PlanningTime>) -> Self {
        Self { date, time }
    }

    #[must_use]
    pub const fn on(date: PlanningDate) -> Self {
        Self::new(Some(date), None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct MonthDay {
    pub month: u8,
    pub day: u8,
}

impl MonthDay {
    #[must_use]
    pub const fn new(month: u8, day: u8) -> Option<Self> {
        if month >= 1 && month <= 12 && day >= 1 && day <= days_in_month(2024, month) {
            Some(Self { month, day })
        } else {
            None
        }
    }
}

impl From<PlanningDate> for MonthDay {
    fn from(value: PlanningDate) -> Self {
        Self {
            month: value.month,
            day: value.day,
        }
    }
}

impl Display for MonthDay {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}-{:02}", self.month, self.day)
    }
}

impl FromStr for MonthDay {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = raw.trim().split('-');
        let month = parts
            .next()
            .ok_or_else(|| "month-day must be MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        let day = parts
            .next()
            .ok_or_else(|| "month-day must be MM-DD".to_owned())?
            .parse::<u8>()
            .map_err(|error| error.to_string())?;
        if parts.next().is_some() {
            return Err("month-day must be MM-DD".to_owned());
        }
        Self::new(month, day).ok_or_else(|| "invalid recurring month-day".to_owned())
    }
}

impl Serialize for MonthDay {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MonthDay {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MonthDayVisitor;

        impl Visitor<'_> for MonthDayVisitor {
            type Value = MonthDay;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("an MM-DD recurring month-day")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<MonthDay>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(MonthDayVisitor)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    const fn bit(self) -> u8 {
        1 << self.index()
    }

    const fn index(self) -> u8 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }

    const fn from_index(index: u8) -> Self {
        match index % 7 {
            0 => Self::Monday,
            1 => Self::Tuesday,
            2 => Self::Wednesday,
            3 => Self::Thursday,
            4 => Self::Friday,
            5 => Self::Saturday,
            6 => Self::Sunday,
            _ => unreachable!(),
        }
    }

    fn parse_token(raw: &str) -> std::result::Result<Self, String> {
        match weekday_atom(raw).as_str() {
            "mon" | "monday" => Ok(Self::Monday),
            "tue" | "tues" | "tuesday" => Ok(Self::Tuesday),
            "wed" | "weds" | "wednesday" => Ok(Self::Wednesday),
            "thu" | "thur" | "thurs" | "thursday" => Ok(Self::Thursday),
            "fri" | "friday" => Ok(Self::Friday),
            "sat" | "saturday" => Ok(Self::Saturday),
            "sun" | "sunday" => Ok(Self::Sunday),
            _ => Err(format!("invalid weekday {raw:?}")),
        }
    }
}

impl Display for Weekday {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Monday => "monday",
            Self::Tuesday => "tuesday",
            Self::Wednesday => "wednesday",
            Self::Thursday => "thursday",
            Self::Friday => "friday",
            Self::Saturday => "saturday",
            Self::Sunday => "sunday",
        })
    }
}

impl FromStr for Weekday {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse_token(raw)
    }
}

impl Serialize for Weekday {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Weekday {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct WeekdayVisitor;

        impl Visitor<'_> for WeekdayVisitor {
            type Value = Weekday;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("a weekday name or abbreviation")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<Weekday>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(WeekdayVisitor)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WeekdaySet(u8);

impl WeekdaySet {
    const ALL_BITS: u8 = 0b0111_1111;
    const WEEKDAYS: [Weekday; 5] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
    ];
    const WEEKENDS: [Weekday; 2] = [Weekday::Saturday, Weekday::Sunday];

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn contains(self, weekday: Weekday) -> bool {
        self.0 & weekday.bit() != 0
    }

    #[must_use]
    pub const fn union(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }

    const fn insert(&mut self, weekday: Weekday) {
        self.0 |= weekday.bit();
    }

    fn insert_many(&mut self, weekdays: impl IntoIterator<Item = Weekday>) {
        weekdays
            .into_iter()
            .for_each(|weekday| self.insert(weekday));
    }

    fn insert_range(&mut self, from: Weekday, to: Weekday) {
        let mut index = from.index();
        loop {
            let weekday = Weekday::from_index(index);
            self.insert(weekday);
            if weekday == to {
                break;
            }
            index = (index + 1) % 7;
        }
    }

    fn ingest_token(&mut self, raw: &str) -> std::result::Result<(), String> {
        let token = weekday_atom(raw);
        match token.as_str() {
            "" | "none" => Ok(()),
            "all" | "daily" | "everyday" => {
                self.0 = Self::ALL_BITS;
                Ok(())
            }
            "weekday" | "weekdays" => {
                self.insert_many(Self::WEEKDAYS);
                Ok(())
            }
            "weekend" | "weekends" => {
                self.insert_many(Self::WEEKENDS);
                Ok(())
            }
            _ => {
                if let Some((from, to)) = token.split_once('-') {
                    self.insert_range(Weekday::parse_token(from)?, Weekday::parse_token(to)?);
                } else {
                    self.insert(Weekday::parse_token(&token)?);
                }
                Ok(())
            }
        }
    }

    fn iter(self) -> impl Iterator<Item = Weekday> {
        Weekday::ALL
            .into_iter()
            .filter(move |weekday| self.contains(*weekday))
    }
}

impl FromStr for WeekdaySet {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        let mut set = Self::empty();
        for token in
            raw.split(|c: char| c == ',' || c == ';' || c == '|' || c == '/' || c.is_whitespace())
        {
            set.ingest_token(token)?;
        }
        Ok(set)
    }
}

impl Serialize for WeekdaySet {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let weekdays = self.iter().collect::<Vec<_>>();
        weekdays.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WeekdaySet {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct WeekdaySetVisitor;

        impl<'de> Visitor<'de> for WeekdaySetVisitor {
            type Value = WeekdaySet;

            fn expecting(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str("a weekday string or sequence")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse::<WeekdaySet>().map_err(E::custom)
            }

            fn visit_seq<A>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut set = WeekdaySet::empty();
                while let Some(raw) = seq.next_element::<String>()? {
                    set = set.union(
                        raw.parse::<WeekdaySet>()
                            .map_err(serde::de::Error::custom)?,
                    );
                }
                Ok(set)
            }
        }

        deserializer.deserialize_any(WeekdaySetVisitor)
    }
}

fn weekday_atom(raw: &str) -> String {
    raw.trim()
        .trim_matches('.')
        .to_ascii_lowercase()
        .replace('_', "-")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SeasonalWindow {
    pub from: MonthDay,
    pub to: MonthDay,
}

impl SeasonalWindow {
    #[must_use]
    pub const fn new(from: MonthDay, to: MonthDay) -> Self {
        Self { from, to }
    }

    #[must_use]
    pub fn contains(self, date: PlanningDate) -> bool {
        let day = MonthDay::from(date);
        if self.from <= self.to {
            self.from <= day && day <= self.to
        } else {
            self.from <= day || day <= self.to
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct AccessWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PlanningDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<PlanningDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seasonal: Option<SeasonalWindow>,
    #[serde(default, skip_serializing_if = "WeekdaySet::is_empty")]
    pub weekdays: WeekdaySet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<DailyTimeWindow>,
}

impl AccessWindow {
    #[must_use]
    pub const fn is_always(&self) -> bool {
        self.from.is_none()
            && self.to.is_none()
            && self.seasonal.is_none()
            && self.weekdays.is_empty()
            && self.time.is_none()
    }

    #[must_use]
    pub fn contains(self, date: Option<PlanningDate>) -> bool {
        self.contains_at(Some(PlanningMoment::new(date, None)))
    }

    #[must_use]
    pub fn contains_at(self, moment: Option<PlanningMoment>) -> bool {
        let Some(moment) = moment else {
            return true;
        };
        moment.date.is_none_or(|date| {
            self.from.is_none_or(|from| from <= date)
                && self.to.is_none_or(|to| date <= to)
                && self.seasonal.is_none_or(|season| season.contains(date))
                && (self.weekdays.is_empty() || self.weekdays.contains(date.weekday()))
        }) && self
            .time
            .is_none_or(|time_window| moment.time.is_none_or(|time| time_window.contains(time)))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessOverlay {
    pub name: String,
    pub access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub travel: Option<EdgeTravel>,
    #[serde(default, skip_serializing_if = "AccessWindow::is_always")]
    pub active: AccessWindow,
    pub confidence: f64,
    pub tolerance_m: f64,
    pub provenance: Provenance,
    pub geometry: OverlayGeometry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TerrainOverlay {
    pub name: String,
    pub terrain: Terrain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    pub confidence: f64,
    pub tolerance_m: f64,
    pub provenance: Provenance,
    pub geometry: OverlayGeometry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextOverlay {
    pub name: String,
    pub kind: CrossingKind,
    pub confidence: f64,
    pub provenance: Provenance,
    pub geometry: LineString,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum OverlayGeometry {
    Polygon(Vec<Coord>),
    MultiPolygon(Vec<Vec<Coord>>),
    Line(LineString),
    MultiLine(Vec<LineString>),
}

impl OverlayGeometry {
    #[must_use]
    pub fn affects(&self, edge: &Edge, tolerance_m: f64) -> bool {
        let midpoint = edge_midpoint(edge);
        match self {
            Self::Polygon(ring) => point_in_ring(midpoint, ring),
            Self::MultiPolygon(rings) => rings.iter().any(|ring| point_in_ring(midpoint, ring)),
            Self::Line(line) => point_line_distance_m(midpoint, line) <= tolerance_m,
            Self::MultiLine(lines) => lines
                .iter()
                .any(|line| point_line_distance_m(midpoint, line) <= tolerance_m),
        }
    }
}

impl AccessOverlay {
    #[must_use]
    pub fn affects(&self, edge: &Edge) -> bool {
        self.geometry.affects(edge, self.tolerance_m)
    }

    #[must_use]
    pub fn active_on(&self, date: Option<PlanningDate>) -> bool {
        self.active.contains(date)
    }

    #[must_use]
    pub fn active_at(&self, moment: Option<PlanningMoment>) -> bool {
        self.active.contains_at(moment)
    }
}

impl TerrainOverlay {
    #[must_use]
    pub fn affects(&self, edge: &Edge) -> bool {
        self.geometry.affects(edge, self.tolerance_m)
    }
}

pub fn apply_access_overlays(
    graph: &mut TrailGraph,
    overlays: &[AccessOverlay],
    planning_date: Option<PlanningDate>,
    weights: DifficultyWeights,
) -> usize {
    apply_access_overlays_at(
        graph,
        overlays,
        Some(PlanningMoment::new(planning_date, None)),
        weights,
    )
}

pub fn apply_access_overlays_at(
    graph: &mut TrailGraph,
    overlays: &[AccessOverlay],
    planning_moment: Option<PlanningMoment>,
    weights: DifficultyWeights,
) -> usize {
    let mut touched = 0usize;
    let mut travel_changed = false;
    for edge in &mut graph.edges {
        for overlay in overlays {
            if !overlay.active_at(planning_moment) || !overlay.affects(edge) {
                continue;
            }
            touched += 1;
            edge.attr.access = overlay.access;
            if let Some(travel) = overlay.travel {
                edge.attr.travel = travel;
                travel_changed = true;
            }
            edge.attr.access_confidence = edge.attr.access_confidence.max(overlay.confidence);
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
            if !edge.attr.access_provenance.contains(&overlay.provenance) {
                edge.attr.access_provenance.push(overlay.provenance.clone());
            }
        }
        weights.apply_edge(edge);
    }
    if travel_changed {
        graph.rebuild_adjacency();
    }
    touched
}

pub fn apply_terrain_overlays(
    graph: &mut TrailGraph,
    overlays: &[TerrainOverlay],
    weights: DifficultyWeights,
) -> usize {
    let mut touched = 0usize;
    for edge in &mut graph.edges {
        let mut changed = false;
        for overlay in overlays {
            if !overlay.affects(edge) {
                continue;
            }
            touched += 1;
            changed = true;
            edge.attr.terrain = overlay.terrain;
            if let Some(surface) = &overlay.surface {
                edge.attr.surface = Some(surface.clone());
            }
            edge.attr.terrain_confidence = edge.attr.terrain_confidence.max(overlay.confidence);
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
            push_terrain_evidence(edge, overlay);
        }
        if changed {
            weights.apply_edge(edge);
        }
    }
    touched
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && !year.is_multiple_of(100) || year.is_multiple_of(400)
}

pub fn apply_context_overlays(
    graph: &mut TrailGraph,
    overlays: &[ContextOverlay],
    weights: DifficultyWeights,
) -> usize {
    let mut crossings = 0usize;
    for edge in &mut graph.edges {
        let mut touched = false;
        for overlay in overlays {
            let count = crossing_count(&edge.geometry, &overlay.geometry);
            if count == 0 {
                continue;
            }
            touched = true;
            crossings += usize::try_from(count).unwrap_or(usize::MAX);
            push_crossing(edge, overlay, count);
            if overlay.kind == CrossingKind::Road {
                edge.attr.road_exposure =
                    edge.attr.road_exposure.max(road_crossing_exposure(count));
            }
            edge.attr.confidence = edge.attr.confidence.min(overlay.confidence);
        }
        if touched {
            weights.apply_edge(edge);
        }
    }
    crossings
}

fn push_terrain_evidence(edge: &mut Edge, overlay: &TerrainOverlay) {
    let rationale = "terrain overlay";
    if let Some(existing) = edge.attr.terrain_evidence.iter_mut().find(|x| {
        x.terrain == overlay.terrain
            && x.provenance.as_ref() == Some(&overlay.provenance)
            && x.rationale == rationale
    }) {
        existing.confidence = existing.confidence.max(overlay.confidence);
        return;
    }
    edge.attr.terrain_evidence.push(TerrainEvidence {
        terrain: overlay.terrain,
        confidence: overlay.confidence,
        rationale: rationale.to_owned(),
        provenance: Some(overlay.provenance.clone()),
    });
}

#[must_use]
pub fn edge_midpoint(edge: &Edge) -> Coord {
    let points = &edge.geometry.points;
    let mid = points.len() / 2;
    if points.len().is_multiple_of(2) {
        points[mid - 1].lerp(points[mid], 0.5)
    } else {
        points[mid]
    }
}

fn point_in_ring(point: Coord, ring: &[Coord]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let pi = ring[i];
        let pj = ring[j];
        let crosses = (pi.lat > point.lat) != (pj.lat > point.lat);
        if crosses {
            let lon = (pj.lon - pi.lon).mul_add((point.lat - pi.lat) / (pj.lat - pi.lat), pi.lon);
            if point.lon < lon {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn point_line_distance_m(point: Coord, line: &LineString) -> f64 {
    line.points
        .windows(2)
        .map(|segment| point_segment_distance_m(point, segment[0], segment[1]))
        .min_by(f64::total_cmp)
        .unwrap_or(f64::INFINITY)
}

fn crossing_count(a: &LineString, b: &LineString) -> u32 {
    a.points
        .windows(2)
        .map(|lhs| {
            b.points
                .windows(2)
                .filter(|rhs| segments_cross(lhs[0], lhs[1], rhs[0], rhs[1]))
                .count()
        })
        .sum::<usize>()
        .try_into()
        .unwrap_or(u32::MAX)
}

fn segments_cross(a0: Coord, a1: Coord, b0: Coord, b1: Coord) -> bool {
    let d1 = orient(a0, a1, b0);
    let d2 = orient(a0, a1, b1);
    let d3 = orient(b0, b1, a0);
    let d4 = orient(b0, b1, a1);
    if d1.abs() <= 1.0e-12 && on_segment(a0, a1, b0)
        || d2.abs() <= 1.0e-12 && on_segment(a0, a1, b1)
        || d3.abs() <= 1.0e-12 && on_segment(b0, b1, a0)
        || d4.abs() <= 1.0e-12 && on_segment(b0, b1, a1)
    {
        return true;
    }
    (d1 > 0.0) != (d2 > 0.0) && (d3 > 0.0) != (d4 > 0.0)
}

fn orient(a: Coord, b: Coord, c: Coord) -> f64 {
    (b.lon - a.lon).mul_add(c.lat - a.lat, -(b.lat - a.lat) * (c.lon - a.lon))
}

fn on_segment(a: Coord, b: Coord, p: Coord) -> bool {
    (a.lon.min(b.lon) - 1.0e-12..=a.lon.max(b.lon) + 1.0e-12).contains(&p.lon)
        && (a.lat.min(b.lat) - 1.0e-12..=a.lat.max(b.lat) + 1.0e-12).contains(&p.lat)
}

fn push_crossing(edge: &mut Edge, overlay: &ContextOverlay, count: u32) {
    if let Some(existing) = edge
        .attr
        .crossings
        .iter_mut()
        .find(|x| x.kind == overlay.kind && x.provenance == overlay.provenance)
    {
        existing.count = existing.count.max(count);
        return;
    }
    edge.attr.crossings.push(CrossingEvidence {
        kind: overlay.kind,
        count,
        provenance: overlay.provenance.clone(),
    });
}

fn road_crossing_exposure(count: u32) -> f64 {
    (f64::from(count) * 0.03).clamp(0.0, 0.20)
}

fn point_segment_distance_m(point: Coord, start: Coord, end: Coord) -> f64 {
    let lat_scale = 111_320.0;
    let lon_scale = lat_scale * point.lat.to_radians().cos().abs().max(0.01);
    let point_x = point.lon * lon_scale;
    let point_y = point.lat * lat_scale;
    let start_x = start.lon * lon_scale;
    let start_y = start.lat * lat_scale;
    let end_x = end.lon * lon_scale;
    let end_y = end.lat * lat_scale;
    let delta_x = end_x - start_x;
    let delta_y = end_y - start_y;
    let denom = delta_x.mul_add(delta_x, delta_y * delta_y);
    if denom <= f64::EPSILON {
        return (point_x - start_x).hypot(point_y - start_y);
    }
    let projection = ((point_y - start_y).mul_add(delta_y, (point_x - start_x) * delta_x) / denom)
        .clamp(0.0, 1.0);
    let closest_x = delta_x.mul_add(projection, start_x);
    let closest_y = delta_y.mul_add(projection, start_y);
    (point_x - closest_x).hypot(point_y - closest_y)
}

pub fn polygon(ring: Vec<Coord>) -> Result<OverlayGeometry> {
    if ring.len() < 4 {
        return Err(TrailgenError::InvalidGeometry(
            "overlay polygon ring needs at least four coordinates".to_owned(),
        ));
    }
    Ok(OverlayGeometry::Polygon(ring))
}
