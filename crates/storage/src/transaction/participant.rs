//! Internal transaction ownership for engine-managed remote catalogs.
use roastedduck_common::{ByteString, Result};
use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

#[doc(hidden)]
pub trait TransactionParticipant: std::fmt::Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn commit(&self) -> Result<()>;
    fn rollback(&self) -> Result<()>;
}

#[derive(Debug, Default)]
pub(super) struct Participants(Mutex<Vec<(ByteString, Arc<dyn TransactionParticipant>)>>);
impl Participants {
    pub fn get_or_create(
        &self,
        name: &[u8],
        create: impl FnOnce() -> Result<Arc<dyn TransactionParticipant>>,
    ) -> Result<Arc<dyn TransactionParticipant>> {
        let mut entries = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, entry)) = entries.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
            return Ok(entry.clone());
        }
        let entry = create()?;
        entries.push((name.into(), entry.clone()));
        Ok(entry)
    }

    pub fn commit(&self, writer: Option<&[u8]>) -> Result<()> {
        let mut entries = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        // Read-only participants finish before the only database allowed to write.
        // There is no distributed two-phase commit, as in MetaTransaction.
        entries.sort_by_key(|(name, _)| writer.is_some_and(|w| name.eq_ignore_ascii_case(w)));
        for (index, (_, participant)) in entries.iter().enumerate() {
            if let Err(error) = participant.commit() {
                for (_, pending) in &entries[index..] {
                    let _ = pending.rollback();
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        let entries = std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner));
        let mut error = None;
        for (_, participant) in entries.into_iter().rev() {
            if let Err(e) = participant.rollback() {
                error.get_or_insert(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
impl Drop for Participants {
    fn drop(&mut self) {
        let _ = self.rollback();
    }
}
