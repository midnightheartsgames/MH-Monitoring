//! Основной источник кадров: вложенный PresentMon как дочерний процесс (решение D1).
//!
//! Разделение внутри модуля проходит по тому, нужна ли системе настоящая система:
//!
//! * [`decode`], [`csv`], [`diagnosis`], [`command`] — чистые; они и содержат
//!   всё, на чём прежняя реализация обожглась (PLAN.md §2.1, §2.5, §2.6);
//! * [`child`] и [`session`] — работа с процессом и потоками, проверяемая подставным ребёнком.

pub mod child;
pub mod command;
pub mod csv;
pub mod decode;
pub mod diagnosis;
pub mod session;
pub mod source;
