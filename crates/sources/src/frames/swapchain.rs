//! Удержание замера на одной цепочке обмена.
//!
//! Политика общая для обоих источников кадров: PresentMon отдаёт адрес цепочки строкой из CSV,
//! собственный ETW-потребитель — 64-битным указателем из payload. Ключ поэтому параметр типа, а
//! не `String`: одна политика, две формы адреса, ни одной копии кода.
//!
//! Игра может презентить сразу через несколько цепочек — оверлей лаунчера, слой видео, встроенный
//! браузер. Если их перемешать, получится ряд frametime, не принадлежащий ничему: две независимые
//! частоты, сложенные в одну пилу. То же самое подтвердилось в P0 на собственном ETW-потребителе.
//!
//! Политика перенесена из `SwapChainSelector` (PLAN.md §7.4): **первая увиденная цепочка
//! выигрывает**, а сменить её претендент может, только если текущая молчит [`DEFAULT_SILENT_MS`]
//! и претендент доказал себя [`DEFAULT_MIN_FRAMES_TO_SWITCH`] кадрами. Иначе кратковременный слой
//! угонял бы замер на себя.

use std::borrow::Borrow;

use mh_core::Millis;

pub const DEFAULT_SILENT_MS: Millis = 2_000;
pub const DEFAULT_MIN_FRAMES_TO_SWITCH: u32 = 30;

#[derive(Debug, Clone)]
pub struct SwapChainSelector<K = String> {
    selected: Option<K>,
    selected_seen_at_ms: Millis,
    candidate: Option<K>,
    candidate_frames: u32,
    switches: u32,
    silent_ms: Millis,
    min_frames_to_switch: u32,
}

impl<K> Default for SwapChainSelector<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> SwapChainSelector<K> {
    pub fn new() -> Self {
        Self::with_policy(DEFAULT_SILENT_MS, DEFAULT_MIN_FRAMES_TO_SWITCH)
    }

    pub fn with_policy(silent_ms: Millis, min_frames_to_switch: u32) -> Self {
        Self {
            selected: None,
            selected_seen_at_ms: 0,
            candidate: None,
            candidate_frames: 0,
            switches: 0,
            silent_ms,
            min_frames_to_switch,
        }
    }

    /// Цепочка, на которой сейчас держится замер.
    pub fn selected(&self) -> Option<&K> {
        self.selected.as_ref()
    }

    /// Сколько раз замер переезжал на другую цепочку. Диагностика: больше нуля на спокойной
    /// игре означает, что политика выбрана неудачно либо цепочек действительно много.
    pub fn switches(&self) -> u32 {
        self.switches
    }

    /// Полный сброс — при смене цели или начале нового сеанса захвата.
    pub fn reset(&mut self) {
        self.selected = None;
        self.candidate = None;
        self.candidate_frames = 0;
        self.switches = 0;
        self.selected_seen_at_ms = 0;
    }
}

impl<K: PartialEq> SwapChainSelector<K> {
    /// Принять ли кадр этой цепочки.
    ///
    /// Адрес принимается в заимствованной форме (`&str` для `String`, `&u64` для `u64`), чтобы
    /// на горячем пути — кадр той же цепочки — не было ни одной аллокации.
    pub fn accept<Q>(&mut self, address: &Q, now_ms: Millis) -> bool
    where
        K: Borrow<Q>,
        Q: PartialEq + ToOwned<Owned = K> + ?Sized,
    {
        let Some(current) = self.selected.as_ref() else {
            self.selected = Some(address.to_owned());
            self.selected_seen_at_ms = now_ms;
            return true;
        };

        if current.borrow() == address {
            self.selected_seen_at_ms = now_ms;
            self.candidate = None;
            self.candidate_frames = 0;
            return true;
        }

        // Текущая цепочка ещё жива — чужие кадры просто не наши.
        if now_ms.saturating_sub(self.selected_seen_at_ms) < self.silent_ms {
            return false;
        }

        if self.candidate.as_ref().map(Borrow::borrow) != Some(address) {
            self.candidate = Some(address.to_owned());
            self.candidate_frames = 0;
        }
        self.candidate_frames += 1;
        if self.candidate_frames < self.min_frames_to_switch {
            return false;
        }

        self.selected = Some(address.to_owned());
        self.selected_seen_at_ms = now_ms;
        self.candidate = None;
        self.candidate_frames = 0;
        self.switches += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "0x27A34FD9080";
    const OVERLAY: &str = "0x1111AAAA";

    fn selector() -> SwapChainSelector<String> {
        SwapChainSelector::with_policy(2_000, 30)
    }

    #[test]
    fn the_first_swapchain_seen_wins() {
        let mut selector = selector();
        assert!(selector.accept(MAIN, 1_000));
        assert_eq!(selector.selected().map(String::as_str), Some(MAIN));
    }

    #[test]
    fn frames_of_another_chain_are_rejected_while_the_current_one_is_alive() {
        let mut selector = selector();
        selector.accept(MAIN, 1_000);
        assert!(!selector.accept(OVERLAY, 1_100));
        assert!(selector.accept(MAIN, 1_116));
        assert_eq!(selector.switches(), 0);
    }

    /// Кратковременный слой не должен угнать замер: молчания мало, нужны ещё и кадры.
    #[test]
    fn silence_alone_is_not_enough_to_switch() {
        let mut selector = selector();
        selector.accept(MAIN, 1_000);

        // Прошло больше порога молчания, но претендент дал всего несколько кадров.
        for tick in 0..29 {
            assert!(!selector.accept(OVERLAY, 4_000 + tick));
        }
        assert_eq!(selector.selected().map(String::as_str), Some(MAIN));
        assert_eq!(selector.switches(), 0);
    }

    #[test]
    fn a_persistent_newcomer_takes_over_after_silence_and_enough_frames() {
        let mut selector = selector();
        selector.accept(MAIN, 1_000);

        let mut accepted = false;
        for tick in 0..30 {
            accepted = selector.accept(OVERLAY, 4_000 + tick);
        }
        assert!(accepted, "тридцатый кадр претендента принимается");
        assert_eq!(selector.selected().map(String::as_str), Some(OVERLAY));
        assert_eq!(selector.switches(), 1);
    }

    /// Пока текущая цепочка жива, претендент не копит заслуги вовсе.
    #[test]
    fn a_candidate_earns_nothing_while_the_current_chain_keeps_presenting() {
        let mut selector = selector();
        selector.accept(MAIN, 0);
        for tick in 0..1_000 {
            let now = tick * 16;
            selector.accept(MAIN, now);
            assert!(!selector.accept(OVERLAY, now));
        }
        assert_eq!(selector.selected().map(String::as_str), Some(MAIN));
        assert_eq!(selector.switches(), 0);
    }

    /// Два претендента по очереди не складывают свои кадры в общий зачёт.
    #[test]
    fn alternating_candidates_do_not_pool_their_frames() {
        let mut selector = selector();
        selector.accept(MAIN, 1_000);
        for tick in 0..100 {
            let address = if tick % 2 == 0 { OVERLAY } else { "0x2222BBBB" };
            assert!(!selector.accept(address, 4_000 + tick));
        }
        assert_eq!(selector.selected().map(String::as_str), Some(MAIN));
    }

    #[test]
    fn a_reset_forgets_everything() {
        let mut selector = selector();
        selector.accept(MAIN, 1_000);
        selector.reset();
        assert_eq!(selector.selected(), None);
        assert_eq!(selector.switches(), 0);

        // И после сброса первая цепочка снова выигрывает без всяких условий.
        assert!(selector.accept(OVERLAY, 2_000));
        assert_eq!(selector.selected().map(String::as_str), Some(OVERLAY));
    }
}
