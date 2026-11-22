// Historical clean-commit capture and legacy basis readers. Filesystem capture
// runs off the state lock and cannot authorize delivery or create a control obligation.

#[cfg(test)]
use super::*;

#[cfg(test)]
pub(crate) type EngramSourceCapture =
    std::result::Result<Option<EngramExecutionSourceBasis>, SourceBasisRefusal>;

/// At most this many threads of one host take turn source bases at once,
/// healthy and abandoned alike. A capture past it counts as not taken.
pub(crate) const ENGRAM_TURN_BASIS_CAPTURE_LIMIT: usize = 32;
