#![cfg(test)]

use crate::schema_migration::{CURRENT_STORAGE_SCHEMA_VERSION, INITIAL_STORAGE_SCHEMA_VERSION};
use crate::{DataKey, Error, Escrow, EscrowClient};
use soroban_sdk::{testutils::Address as _, testutils::Events, Address, Env};

struct MigrationFixture {
    env: Env,
    contract_id: Address,
    admin: Address,
}

impl MigrationFixture {
    fn initialized() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(Escrow, ());
        let admin = Address::generate(&env);
        EscrowClient::new(&env, &contract_id).initialize(&admin);

        Self {
            env,
            contract_id,
            admin,
        }
    }

    fn client(&self) -> EscrowClient<'_> {
        EscrowClient::new(&self.env, &self.contract_id)
    }

    fn stored_version(&self) -> Option<u32> {
        self.env.as_contract(&self.contract_id, || {
            self.env.storage().persistent().get(&DataKey::SchemaVersion)
        })
    }

    fn overwrite_version(&self, version: u32) {
        self.env.as_contract(&self.contract_id, || {
            self.env
                .storage()
                .persistent()
                .set(&DataKey::SchemaVersion, &version);
        });
    }
}

#[test]
fn legacy_read_is_side_effect_free_until_successful_migration() {
    let fixture = MigrationFixture::initialized();

    assert_eq!(
        fixture.client().get_schema_version(),
        INITIAL_STORAGE_SCHEMA_VERSION
    );
    assert_eq!(fixture.stored_version(), None);
}

#[test]
fn authorized_upgrade_commits_version_and_one_diagnostic_event() {
    let fixture = MigrationFixture::initialized();
    assert_eq!(
        fixture
            .client()
            .migrate_escrow_storage(&fixture.admin, &CURRENT_STORAGE_SCHEMA_VERSION),
        CURRENT_STORAGE_SCHEMA_VERSION
    );
    let events = fixture.env.events().all();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events.last().expect("migration event").0,
        fixture.contract_id
    );
    assert_eq!(
        fixture.stored_version(),
        Some(CURRENT_STORAGE_SCHEMA_VERSION)
    );
}

#[test]
fn duplicate_retry_is_idempotent_and_does_not_emit_a_second_event() {
    let fixture = MigrationFixture::initialized();
    let client = fixture.client();

    assert_eq!(
        client.migrate_escrow_storage(&fixture.admin, &CURRENT_STORAGE_SCHEMA_VERSION),
        CURRENT_STORAGE_SCHEMA_VERSION
    );
    assert_eq!(fixture.env.events().all().len(), 1);

    // A transaction racing the first call is retried against the committed
    // marker. It must observe success without another write or event.
    assert_eq!(
        client.migrate_escrow_storage(&fixture.admin, &CURRENT_STORAGE_SCHEMA_VERSION),
        CURRENT_STORAGE_SCHEMA_VERSION
    );
    assert_eq!(fixture.env.events().all().len(), 0);
    assert_eq!(
        fixture.stored_version(),
        Some(CURRENT_STORAGE_SCHEMA_VERSION)
    );
}

#[test]
fn downgrade_is_rejected_without_mutating_committed_state() {
    let fixture = MigrationFixture::initialized();
    let client = fixture.client();
    client.migrate_escrow_storage(&fixture.admin, &CURRENT_STORAGE_SCHEMA_VERSION);
    assert_eq!(
        client.try_migrate_escrow_storage(&fixture.admin, &INITIAL_STORAGE_SCHEMA_VERSION),
        Err(Ok(Error::InvalidMigrationVersion))
    );
    assert_eq!(fixture.env.events().all().len(), 0);
    assert_eq!(
        fixture.stored_version(),
        Some(CURRENT_STORAGE_SCHEMA_VERSION)
    );
}

#[test]
fn zero_and_future_targets_are_rejected_without_creating_a_marker() {
    for target in [0, CURRENT_STORAGE_SCHEMA_VERSION + 1, u32::MAX] {
        let fixture = MigrationFixture::initialized();
        assert_eq!(
            fixture
                .client()
                .try_migrate_escrow_storage(&fixture.admin, &target),
            Err(Ok(Error::InvalidMigrationVersion))
        );
        assert_eq!(fixture.env.events().all().len(), 0);
        assert_eq!(fixture.stored_version(), None);
    }
}

#[test]
fn corrupt_stored_versions_cannot_be_blessed_by_an_idempotent_retry() {
    for corrupt_version in [0, CURRENT_STORAGE_SCHEMA_VERSION + 1, u32::MAX] {
        let fixture = MigrationFixture::initialized();
        fixture.overwrite_version(corrupt_version);
        assert_eq!(
            fixture
                .client()
                .try_migrate_escrow_storage(&fixture.admin, &corrupt_version),
            Err(Ok(Error::InvalidMigrationVersion))
        );
        assert_eq!(fixture.env.events().all().len(), 0);
        assert_eq!(fixture.stored_version(), Some(corrupt_version));
    }
}

#[test]
fn non_admin_is_rejected_before_any_schema_state_change() {
    let fixture = MigrationFixture::initialized();
    let attacker = Address::generate(&fixture.env);
    assert_eq!(
        fixture
            .client()
            .try_migrate_escrow_storage(&attacker, &CURRENT_STORAGE_SCHEMA_VERSION),
        Err(Ok(Error::UnauthorizedRole))
    );
    assert_eq!(fixture.env.events().all().len(), 0);
    assert_eq!(fixture.stored_version(), None);
}

#[test]
fn uninitialized_contract_rejects_migration_without_writing_state() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(Escrow, ());
    let admin = Address::generate(&env);
    let client = EscrowClient::new(&env, &contract_id);

    assert_eq!(
        client.try_migrate_escrow_storage(&admin, &CURRENT_STORAGE_SCHEMA_VERSION),
        Err(Ok(Error::NotInitialized))
    );
    env.as_contract(&contract_id, || {
        assert_eq!(
            env.storage()
                .persistent()
                .get::<_, u32>(&DataKey::SchemaVersion),
            None
        );
    });
    assert_eq!(env.events().all().len(), 0);
}
