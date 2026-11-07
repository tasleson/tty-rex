// parking_lot.rs
//
// Copyright 2023 Christian Hergert <christian@sourceandstack.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Recently closed tabs, kept alive for a few seconds so `win.undo-close-tab`
//! can bring them back (`src/ptyxis-parking-lot.c`).
//!
//! The lot only does the bookkeeping. Its owner arms a timer for each
//! [`ParkingLot::push`] and hands the returned serial back to
//! [`ParkingLot::expire`] when it fires, so a tab that was reopened (and
//! maybe closed again) in the meantime is not taken by a stale timer.

use std::collections::VecDeque;
use std::time::Duration;

/// How long a closed tab can be reopened (`DEFAULT_TIMEOUT_SECONDS`).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Identifies one stay in the lot.
pub type Serial = u64;

#[derive(Debug)]
pub struct ParkingLot<T> {
    items: VecDeque<(Serial, T)>,
    next_serial: Serial,
}

impl<T> Default for ParkingLot<T> {
    fn default() -> Self {
        Self {
            items: VecDeque::new(),
            next_serial: 0,
        }
    }
}

impl<T> ParkingLot<T> {
    /// Parks `item` and returns the serial its timer expires
    /// (`ptyxis_parking_lot_push()`).
    pub fn push(&mut self, item: T) -> Serial {
        let serial = self.next_serial;
        self.next_serial += 1;
        self.items.push_back((serial, item));
        serial
    }

    /// Takes the most recently parked item back out
    /// (`ptyxis_parking_lot_pop()`). Ptyxis's GQueue hands out the oldest
    /// one instead, which contradicts its own "Undo closing the last tab"
    /// shortcut description; tty-rex follows the description.
    pub fn pop(&mut self) -> Option<T> {
        self.items.pop_back().map(|(_, item)| item)
    }

    /// Removes the item parked under `serial` once its time is up
    /// (`ptyxis_parking_lot_source_func()`); `None` if it already left.
    pub fn expire(&mut self, serial: Serial) -> Option<T> {
        let index = self.items.iter().position(|(s, _)| *s == serial)?;
        self.items.remove(index).map(|(_, item)| item)
    }

    /// Empties the lot, e.g. when its window goes away
    /// (`ptyxis_parking_lot_dispose()`).
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        self.items.drain(..).map(|(_, item)| item)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Push(char),
        /// Pops, expecting this item.
        Pop(Option<char>),
        /// Expires the serial of the nth push, expecting this item.
        Expire(usize, Option<char>),
        /// Drains, expecting these items in parking order.
        Drain(&'static str),
    }

    #[test]
    fn operations() {
        use Op::*;
        let cases: &[(&str, &[Op], &str)] = &[
            ("empty pop", &[Pop(None)], ""),
            (
                "pop newest first",
                &[Push('a'), Push('b'), Pop(Some('b'))],
                "a",
            ),
            (
                "pop until empty",
                &[
                    Push('a'),
                    Push('b'),
                    Pop(Some('b')),
                    Pop(Some('a')),
                    Pop(None),
                ],
                "",
            ),
            (
                "expire middle",
                &[Push('a'), Push('b'), Push('c'), Expire(1, Some('b'))],
                "ac",
            ),
            (
                "expire after pop is stale",
                &[Push('a'), Pop(Some('a')), Expire(0, None)],
                "",
            ),
            (
                "reparked item gets a fresh serial",
                &[Push('a'), Pop(Some('a')), Push('a'), Expire(0, None)],
                "a",
            ),
            (
                "expire twice",
                &[Push('a'), Expire(0, Some('a')), Expire(0, None)],
                "",
            ),
            (
                "drain keeps order",
                &[Push('a'), Push('b'), Drain("ab"), Pop(None)],
                "",
            ),
        ];
        for (name, ops, remaining) in cases {
            let mut lot = ParkingLot::default();
            let mut serials = Vec::new();
            for op in *ops {
                match *op {
                    Push(item) => serials.push(lot.push(item)),
                    Pop(expected) => assert_eq!(lot.pop(), expected, "{name}"),
                    Expire(nth, expected) => {
                        assert_eq!(lot.expire(serials[nth]), expected, "{name}")
                    }
                    Drain(expected) => {
                        assert_eq!(lot.drain().collect::<String>(), expected, "{name}")
                    }
                }
            }
            assert_eq!(lot.len(), remaining.len(), "{name}");
            assert_eq!(lot.is_empty(), remaining.is_empty(), "{name}");
            assert_eq!(lot.drain().collect::<String>(), *remaining, "{name}");
        }
    }
}
