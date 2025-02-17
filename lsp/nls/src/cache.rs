use std::{collections::HashMap, ffi::OsString, io};

use codespan::FileId;
use nickel::{
    cache::{Cache, CacheError, CacheOp, CachedTerm, EntryState},
    error::TypecheckError,
    typecheck,
};

use crate::linearization::{completed::Completed, AnalysisHost};

pub trait CacheExt {
    fn update_content(&mut self, path: impl Into<OsString>, s: String) -> io::Result<FileId>;
    fn typecheck_with_analysis(
        &mut self,
        file_id: FileId,
        global_env: &typecheck::Environment,
        lin_cache: &mut HashMap<FileId, Completed>,
    ) -> Result<CacheOp<()>, CacheError<TypecheckError>>;
}

impl CacheExt for Cache {
    fn update_content(&mut self, path: impl Into<OsString>, source: String) -> io::Result<FileId> {
        let path: OsString = path.into();
        if let Some(file_id) = self.id_of(path.clone()) {
            self.files_mut().update(file_id, source);
            // invalidate cache so the file gets parsed again
            self.terms_mut().remove(&file_id);
            Ok(file_id)
        } else {
            Ok(self.add_string(path, source))
        }
    }
    fn typecheck_with_analysis<'a>(
        &mut self,
        file_id: FileId,
        global_env: &typecheck::Environment,
        lin_cache: &mut HashMap<FileId, Completed>,
    ) -> Result<CacheOp<()>, CacheError<TypecheckError>> {
        if !self.terms_mut().contains_key(&file_id) {
            return Err(CacheError::NotParsed);
        }

        // After self.parse(), the cache must be populated
        let CachedTerm { term, state, .. } = self.terms().get(&file_id).unwrap();

        if *state > EntryState::Typechecked && lin_cache.contains_key(&file_id) {
            Ok(CacheOp::Cached(()))
        } else if *state >= EntryState::Parsed {
            let host = AnalysisHost::new();
            let (_, linearized) = typecheck::type_check(term, global_env, self, host)?;
            self.update_state(file_id, EntryState::Typechecked);
            lin_cache.insert(file_id, linearized);
            Ok(CacheOp::Done(()))
        } else {
            panic!()
        }
    }
}
