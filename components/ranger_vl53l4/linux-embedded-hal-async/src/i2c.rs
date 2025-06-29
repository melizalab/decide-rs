use std::{
    error,
    fmt::{self, Display, Formatter},
    ops::{Deref, DerefMut},
    panic,
};

use async_scoped::TokioScope;
use embedded_hal_async::i2c::{
    self, ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation, SevenBitAddress, TenBitAddress,
};
use i2cdev::{
    core::{I2CMessage, I2CTransfer},
    linux::{I2CMessageFlags, LinuxI2CBus, LinuxI2CError, LinuxI2CMessage},
};
use nix::errno::Errno;

#[derive(Debug)]
pub struct LinuxI2cError(LinuxI2CError);

impl i2c::Error for LinuxI2cError {
    fn kind(&self) -> ErrorKind {
        match self.0 {
            LinuxI2CError::Errno(err) => match Errno::from_raw(err) {
                // Best effort mapping of `nix::errno::Errno` to
                // `embedded_hal_async::i2c::ErrorKind` according to
                // https://docs.kernel.org/i2c/fault-codes.html.
                Errno::EBADMSG => ErrorKind::Bus,
                Errno::EBUSY => ErrorKind::Bus,
                Errno::EIO => ErrorKind::Bus,
                Errno::ETIMEDOUT => ErrorKind::Bus,
                Errno::EAGAIN => ErrorKind::ArbitrationLoss,
                Errno::ENXIO => ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address),
                Errno::ENODEV => ErrorKind::NoAcknowledge(NoAcknowledgeSource::Data),
                Errno::ENOMEM => ErrorKind::Overrun,
                _ => ErrorKind::Other,
            },
            _ => ErrorKind::Other,
        }
    }
}

impl From<LinuxI2CError> for LinuxI2cError {
    fn from(value: LinuxI2CError) -> Self {
        Self(value)
    }
}

impl Display for LinuxI2cError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Linux I2C error: {}", self.0)
    }
}

impl error::Error for LinuxI2cError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        Some(&self.0)
    }
}

pub struct LinuxI2c(LinuxI2CBus);

impl LinuxI2c {
    pub fn new(bus: LinuxI2CBus) -> Self {
        Self(bus)
    }

    fn transact(
        &mut self,
        address: u16,
        operations: &mut [Operation<'_>],
        _flags: I2CMessageFlags,
    ) -> Result<(), LinuxI2cError> {
        let (_, mut vec) = TokioScope::scope_and_block(|scope| {
            scope.spawn_blocking(|| {
                let mut msgs: Box<[LinuxI2CMessage]> = operations
                    .into_iter()
                    .map(|item| match item {
                        Operation::Read(data) => LinuxI2CMessage::read(data)
                            .with_address(address),
                        Operation::Write(data) => LinuxI2CMessage::write(data)
                            .with_address(address)
                    })
                    .collect();
                self.0.transfer(&mut msgs)
            })
        });
        match vec.pop().unwrap() {
            Ok(result) => result?,
            Err(err) => panic::resume_unwind(err.into_panic()),
        };
        Ok(())
    }
}

impl ErrorType for LinuxI2c {
    type Error = LinuxI2cError;
}

impl I2c<SevenBitAddress> for LinuxI2c {
    async fn transaction(
        &mut self,
        address: SevenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.transact(address as u16, operations, I2CMessageFlags::empty())
    }
}

impl I2c<TenBitAddress> for LinuxI2c {
    async fn transaction(
        &mut self,
        address: TenBitAddress,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        self.transact(
            address,
            operations,
            // Flags to enable 10-bit address mode. Not sure if this is sufficient to enable 10-bit
            // address mode.
            I2CMessageFlags::TEN_BIT_ADDRESS,
        )
    }
}

impl Deref for LinuxI2c {
    type Target = LinuxI2CBus;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for LinuxI2c {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}