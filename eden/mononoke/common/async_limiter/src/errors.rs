/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ErrorKind {
    #[error("Runtime is shutting down")]
    RuntimeShuttingDown,
}
