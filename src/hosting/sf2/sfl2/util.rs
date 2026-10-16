	pub(crate) trait TryPush<T> {
		fn try_push(&mut self, item: T) -> Result<(), std::collections::TryReserveError>;
	}

	impl<T> TryPush<T> for Vec<T> {
		fn try_push(&mut self, item: T) -> Result<(), std::collections::TryReserveError> {
			self.try_reserve(1)?;
			self.push(item);
			Ok(())
		}
	}