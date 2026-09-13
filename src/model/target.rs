//! Target ABI observations are published together and owned by the debugger.

use super::DebuggerModel;
use crate::debugger::{TargetArchitecture, TargetEndian};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TargetAbi {
    architecture: TargetArchitecture,
    endian: Option<TargetEndian>,
    pointer_bits: u32,
    pointer_bits_known: bool,
}

impl Default for TargetAbi {
    fn default() -> Self {
        Self {
            architecture: TargetArchitecture::Unknown,
            endian: None,
            pointer_bits: usize::BITS,
            pointer_bits_known: false,
        }
    }
}

impl DebuggerModel {
    pub(crate) fn target_architecture(&self) -> TargetArchitecture {
        self.target.get().architecture
    }

    pub(crate) fn target_endian(&self) -> Option<TargetEndian> {
        self.target.get().endian
    }

    /// Presentation may use an inferred width; memory operations can require
    /// `known_target_pointer_bits` instead of treating the host as evidence.
    pub(crate) fn target_pointer_bits(&self) -> u32 {
        self.target.get().pointer_bits
    }

    pub(crate) fn known_target_pointer_bits(&self) -> Option<u32> {
        let target = self.target.get();
        target.pointer_bits_known.then_some(target.pointer_bits)
    }

    pub(crate) fn set_target_endian(&self, endian: Option<TargetEndian>) {
        self.set_target_abi(TargetAbi {
            endian,
            ..self.target.get()
        });
    }

    pub(crate) fn set_target_architecture(&self, architecture: TargetArchitecture) {
        let mut target = self.target.get();

        target.architecture = if target.pointer_bits_known {
            architecture.refine_for_pointer_bits(target.pointer_bits)
        } else {
            if let Some(bits) = architecture.pointer_bits() {
                target.pointer_bits = bits;
            }

            architecture
        };

        self.set_target_abi(target);
    }

    pub(crate) fn set_target_pointer_bits(&self, bits: u32) -> bool {
        if !matches!(bits, 32 | 64) {
            return false;
        }

        let target = self.target.get();

        self.set_target_abi(TargetAbi {
            pointer_bits: bits,
            pointer_bits_known: true,
            architecture: target.architecture.refine_for_pointer_bits(bits),
            ..target
        });

        true
    }

    fn set_target_abi(&self, target: TargetAbi) {
        let previous = self.target.replace(target);

        if previous.architecture != TargetArchitecture::Unknown
            && target.architecture != TargetArchitecture::Unknown
            && previous.architecture != target.architecture
        {
            self.clear_register_names();
        }
    }

    pub(crate) fn reset_target_abi(&self) {
        self.target.set(TargetAbi::default());
        self.clear_register_names();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DebuggerStateDelta, TargetConnection};

    #[test]
    fn abi_updates_preserve_explicit_widths_and_reject_invalid_observations() {
        let model = DebuggerModel::new(None);
        assert_eq!(model.known_target_pointer_bits(), None);
        model.set_target_architecture(TargetArchitecture::X86);
        assert_eq!(model.target_pointer_bits(), 32);
        assert_eq!(model.known_target_pointer_bits(), None);
        assert!(model.set_target_pointer_bits(32));
        model.set_target_architecture(TargetArchitecture::X86_64);
        assert_eq!(model.target_architecture(), TargetArchitecture::X86_64);
        assert_eq!(model.known_target_pointer_bits(), Some(32));
        model.set_target_endian(Some(TargetEndian::Big));
        let before = model.target.get();

        for bits in [0, 16, 65, 128, u32::MAX] {
            assert!(!model.set_target_pointer_bits(bits));
            assert_eq!(model.target.get(), before);
        }

        model.reset_target_abi();
        model.set_target_architecture(TargetArchitecture::RiscV32);
        model.cache_register_names(std::rc::Rc::new(vec![String::from("pc")]));
        assert!(model.set_target_pointer_bits(64));
        assert_eq!(model.target_architecture(), TargetArchitecture::RiscV64);
        assert!(model.cached_register_names().is_none());
    }

    #[test]
    fn target_replacement_and_backend_loss_invalidate_abi_without_a_ui() {
        let model = DebuggerModel::new(None);
        model.set_target_architecture(TargetArchitecture::AArch64);
        model.set_target_pointer_bits(32);
        model.set_target_endian(Some(TargetEndian::Little));
        model.apply_debugger_state_delta(DebuggerStateDelta::establish_connection(
            TargetConnection::Remote,
        ));
        assert_eq!(model.target.get(), TargetAbi::default());
        model.set_target_pointer_bits(32);
        model.set_controls_ready(false);
        assert_eq!(model.target.get(), TargetAbi::default());
    }

    #[test]
    fn inferior_selection_and_exit_retire_abi_observations() {
        let model = DebuggerModel::new(None);
        let record = crate::debugger::parse_record(
            r#"^done,groups=[{id="i1",pid="1234"},{id="i2",pid="1235"}]"#,
        )
        .unwrap();

        model.show_inferiors(crate::debugger::inferiors(&record, None));
        model.set_selected_inferior("i1");
        model.set_target_pointer_bits(32);
        assert!(model.set_selected_inferior("i1"));
        assert_eq!(model.known_target_pointer_bits(), Some(32));
        assert!(model.set_selected_inferior("i2"));
        assert_eq!(model.target.get(), TargetAbi::default());
        model.set_target_pointer_bits(64);
        model.record_inferior_exited("i1");
        assert_eq!(model.known_target_pointer_bits(), Some(64));
        model.record_inferior_exited("i2");
        assert_eq!(model.target.get(), TargetAbi::default());
    }
}
