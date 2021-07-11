// This file is part of Substrate.

// Copyright (C) 2019-2021 Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! # Membership Module
//!
//! Allows control of membership of a set of `AccountId`s, useful for managing membership of of a
//! collective. A prime member may be set

// Ensure we're `no_std` when compiling for Wasm.
#![cfg_attr(not(feature = "std"), no_std)]

use sp_std::prelude::*;
use frame_support::{
	decl_module, decl_storage, decl_event, decl_error,
	traits::{ChangeMembers, InitializeMembers, EnsureOrigin, Contains, SortedMembers, Get},
};
use frame_system::ensure_signed;

pub mod weights;
pub use weights::WeightInfo;

pub trait Config<I = DefaultInstance>: frame_system::Config {
	/// The overarching event type.
	type Event: From<Event<Self, I>> + Into<<Self as frame_system::Config>::Event>;

	/// Required origin for adding a member (though can always be Root).
	type AddOrigin: EnsureOrigin<Self::Origin>;

	/// Required origin for removing a member (though can always be Root).
	type RemoveOrigin: EnsureOrigin<Self::Origin>;

	/// Required origin for adding and removing a member in a single action.
	type SwapOrigin: EnsureOrigin<Self::Origin>;

	/// Required origin for resetting membership.
	type ResetOrigin: EnsureOrigin<Self::Origin>;

	/// Required origin for setting or resetting the prime member.
	type PrimeOrigin: EnsureOrigin<Self::Origin>;

	/// The receiver of the signal for when the membership has been initialized. This happens pre-
	/// genesis and will usually be the same as `MembershipChanged`. If you need to do something
	/// different on initialization, then you can change this accordingly.
	type MembershipInitialized: InitializeMembers<Self::AccountId>;

	/// The receiver of the signal for when the membership has changed.
	type MembershipChanged: ChangeMembers<Self::AccountId>;

	/// The maximum number of members that this membership can have.
	///
	/// This is used for benchmarking. Re-run the benchmarks if this changes.
	///
	/// This is not enforced in the code; the membership size can exceed this limit.
	type MaxMembers: Get<u32>;

	/// Weight information for extrinsics in this pallet.
	type WeightInfo: WeightInfo;
}

decl_storage! {
	trait Store for Module<T: Config<I>, I: Instance=DefaultInstance> as Membership {
		/// The current membership, stored as an ordered Vec.
		Members get(fn members): Vec<T::AccountId>;

		/// The current prime member, if one exists.
		Prime get(fn prime): Option<T::AccountId>;
	}
	add_extra_genesis {
		config(members): Vec<T::AccountId>;
		config(phantom): sp_std::marker::PhantomData<I>;
		build(|config: &Self| {
			let mut members = config.members.clone();

			// TODO: tests
			// Assert that members are unique.
			let has_dupes = (1..members.len())
				.any(|i| members[i..].contains(&members[i - 1]));
			assert!(!has_dupes, "Members cannot contain duplicate accounts.");

			members.sort();
			T::MembershipInitialized::initialize_members(&members);
			<Members<T, I>>::put(members);
		})
	}
}

decl_event!(
	pub enum Event<T, I=DefaultInstance> where
		<T as frame_system::Config>::AccountId,
		<T as Config<I>>::Event,
	{
		/// The given member was added; see the transaction for who.
		MemberAdded,
		/// The given member was removed; see the transaction for who.
		MemberRemoved,
		/// Two members were swapped; see the transaction for who.
		MembersSwapped,
		/// The membership was reset; see the transaction for who the new set is.
		MembersReset,
		/// One of the members' keys changed.
		KeyChanged,
		/// Phantom member, never used.
		Dummy(sp_std::marker::PhantomData<(AccountId, Event)>),
	}
);

decl_error! {
	/// Error for the nicks module.
	pub enum Error for Module<T: Config<I>, I: Instance> {
		/// Already a member.
		AlreadyMember,
		/// Not a member.
		NotMember,
	}
}

decl_module! {
	pub struct Module<T: Config<I>, I: Instance=DefaultInstance>
		for enum Call
		where origin: T::Origin
	{
		type Error = Error<T, I>;

		fn deposit_event() = default;

		/// Add a member `who` to the set.
		///
		/// May only be called from `T::AddOrigin`.
		#[weight = 50_000_000]
		pub fn add_member(origin, who: T::AccountId) {
			T::AddOrigin::ensure_origin(origin)?;

			let mut members = <Members<T, I>>::get();
			let location = members.binary_search(&who).err().ok_or(Error::<T, I>::AlreadyMember)?;
			members.insert(location, who.clone());

			Self::maybe_warn_max_members(&members);
			<Members<T, I>>::put(&members);

			T::MembershipChanged::change_members_sorted(&[who], &[], &members[..]);

			Self::deposit_event(RawEvent::MemberAdded);
		}

		/// Remove a member `who` from the set.
		///
		/// May only be called from `T::RemoveOrigin`.
		#[weight = 50_000_000]
		pub fn remove_member(origin, who: T::AccountId) {
			T::RemoveOrigin::ensure_origin(origin)?;

			let mut members = <Members<T, I>>::get();
			let location = members.binary_search(&who).ok().ok_or(Error::<T, I>::NotMember)?;
			members.remove(location);

			Self::maybe_warn_max_members(&members);
			<Members<T, I>>::put(&members);

			T::MembershipChanged::change_members_sorted(&[], &[who], &members[..]);
			Self::rejig_prime(&members);

			Self::deposit_event(RawEvent::MemberRemoved);
		}

		/// Swap out one member `remove` for another `add`.
		///
		/// May only be called from `T::SwapOrigin`.
		///
		/// Prime membership is *not* passed from `remove` to `add`, if extant.
		#[weight = 50_000_000]
		pub fn swap_member(origin, remove: T::AccountId, add: T::AccountId) {
			T::SwapOrigin::ensure_origin(origin)?;

			if remove == add { return Ok(()) }

			let mut members = <Members<T, I>>::get();
			let location = members.binary_search(&remove).ok().ok_or(Error::<T, I>::NotMember)?;
			let _ = members.binary_search(&add).err().ok_or(Error::<T, I>::AlreadyMember)?;
			members[location] = add.clone();
			members.sort();

			Self::maybe_warn_max_members(&members);
			<Members<T, I>>::put(&members);

			T::MembershipChanged::change_members_sorted(
				&[add],
				&[remove],
				&members[..],
			);
			Self::rejig_prime(&members);

			Self::deposit_event(RawEvent::MembersSwapped);
		}

		/// Change the membership to a new set, disregarding the existing membership. Be nice and
		/// pass `members` pre-sorted.
		///
		/// May only be called from `T::ResetOrigin`.
		#[weight = 50_000_000]
		pub fn reset_members(origin, members: Vec<T::AccountId>) {
			T::ResetOrigin::ensure_origin(origin)?;

			let mut members = members;
			members.sort();
			<Members<T, I>>::mutate(|m| {
				T::MembershipChanged::set_members_sorted(&members[..], m);
				Self::rejig_prime(&members);
				Self::maybe_warn_max_members(&members);
				*m = members;
			});

			Self::deposit_event(RawEvent::MembersReset);
		}

		/// Swap out the sending member for some other key `new`.
		///
		/// May only be called from `Signed` origin of a current member.
		///
		/// Prime membership is passed from the origin account to `new`, if extant.
		#[weight = 50_000_000]
		pub fn change_key(origin, new: T::AccountId) {
			let remove = ensure_signed(origin)?;

			if remove != new {
				let mut members = <Members<T, I>>::get();
				let location = members.binary_search(&remove).ok().ok_or(Error::<T, I>::NotMember)?;
				let _ = members.binary_search(&new).err().ok_or(Error::<T, I>::AlreadyMember)?;
				members[location] = new.clone();
				members.sort();

				Self::maybe_warn_max_members(&members);
				<Members<T, I>>::put(&members);

				T::MembershipChanged::change_members_sorted(
					&[new.clone()],
					&[remove.clone()],
					&members[..],
				);

				if Prime::<T, I>::get() == Some(remove) {
					Prime::<T, I>::put(&new);
					T::MembershipChanged::set_prime(Some(new));
				}
			}

			Self::deposit_event(RawEvent::KeyChanged);
		}

		/// Set the prime member. Must be a current member.
		///
		/// May only be called from `T::PrimeOrigin`.
		#[weight = 50_000_000]
		pub fn set_prime(origin, who: T::AccountId) {
			T::PrimeOrigin::ensure_origin(origin)?;
			Self::members().binary_search(&who).ok().ok_or(Error::<T, I>::NotMember)?;
			Prime::<T, I>::put(&who);
			T::MembershipChanged::set_prime(Some(who));
		}

		/// Remove the prime member if it exists.
		///
		/// May only be called from `T::PrimeOrigin`.
		#[weight = 50_000_000]
		pub fn clear_prime(origin) {
			T::PrimeOrigin::ensure_origin(origin)?;
			Prime::<T, I>::kill();
			T::MembershipChanged::set_prime(None);
		}
	}
}

impl<T: Config<I>, I: Instance> Module<T, I> {
	fn rejig_prime(members: &[T::AccountId]) {
		if let Some(prime) = Prime::<T, I>::get() {
			match members.binary_search(&prime) {
				Ok(_) => T::MembershipChanged::set_prime(Some(prime)),
				Err(_) => Prime::<T, I>::kill(),
			}
		}
	}

	fn maybe_warn_max_members(members: &[T::AccountId]) {
		if members.len() as u32 > T::MaxMembers::get() {
			log::error!(
				target: "runtime::membership",
				"maximum number of members used for weight is exceeded, weights can be underestimated [{} > {}].",
				members.len(),
				T::MaxMembers::get(),
			)
		}
	}
}

impl<T: Config<I>, I: Instance> Contains<T::AccountId> for Module<T, I> {
	fn contains(t: &T::AccountId) -> bool {
		Self::members().binary_search(t).is_ok()
	}
}

impl<T: Config<I>, I: Instance> SortedMembers<T::AccountId> for Module<T, I> {
	fn sorted_members() -> Vec<T::AccountId> {
		Self::members()
	}

	fn count() -> usize {
		Members::<T, I>::decode_len().unwrap_or(0)
	}
}

#[cfg(feature = "runtime-benchmarks")]
mod benchmark {
	use super::{*, Module as Membership};
	use frame_system::RawOrigin;
	use frame_support::{traits::EnsureOrigin, assert_ok};
	use frame_benchmarking::{benchmarks_instance, whitelist, account, impl_benchmark_test_suite};

	const SEED: u32 = 0;

	fn set_members<T: Config<I>, I: Instance>(members: Vec<T::AccountId>, prime: Option<usize>) {
		let reset_origin = T::ResetOrigin::successful_origin();
		let prime_origin = T::PrimeOrigin::successful_origin();

		assert_ok!(<Membership<T, _>>::reset_members(reset_origin, members.clone()));
		if let Some(prime) = prime.map(|i| members[i].clone()) {
			assert_ok!(<Membership<T, _>>::set_prime(prime_origin, prime));
		} else {
			assert_ok!(<Membership<T, _>>::clear_prime(prime_origin));
		}
	}

	benchmarks_instance! {
		add_member {
			let m in 1 .. T::MaxMembers::get();

			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			set_members::<T, I>(members.clone(), None);
			let new_member = account::<T::AccountId>("add", m, SEED);
		}: {
			assert_ok!(<Membership<T, _>>::add_member(T::AddOrigin::successful_origin(), new_member.clone()));
		}
		verify {
			assert!(<Members<T, _>>::get().contains(&new_member));
			#[cfg(test)] crate::tests::clean();
		}

		// the case of no prime or the prime being removed is surely cheaper than the case of
		// reporting a new prime via `MembershipChanged`.
		remove_member {
			let m in 2 .. T::MaxMembers::get();

			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			set_members::<T, I>(members.clone(), Some(members.len() - 1));

			let to_remove = members.first().cloned().unwrap();
		}: {
			assert_ok!(<Membership<T, _>>::remove_member(T::RemoveOrigin::successful_origin(), to_remove.clone()));
		} verify {
			assert!(!<Members<T, _>>::get().contains(&to_remove));
			// prime is rejigged
			assert!(<Prime<T, _>>::get().is_some() && T::MembershipChanged::get_prime().is_some());
			#[cfg(test)] crate::tests::clean();
		}

		// we remove a non-prime to make sure it needs to be set again.
		swap_member {
			let m in 2 .. T::MaxMembers::get();

			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			set_members::<T, I>(members.clone(), Some(members.len() - 1));
			let add = account::<T::AccountId>("member", m, SEED);
			let remove = members.first().cloned().unwrap();
		}: {
			assert_ok!(<Membership<T, _>>::swap_member(
				T::SwapOrigin::successful_origin(),
				remove.clone(),
				add.clone(),
			));
		} verify {
			assert!(!<Members<T, _>>::get().contains(&remove));
			assert!(<Members<T, _>>::get().contains(&add));
			// prime is rejigged
			assert!(<Prime<T, _>>::get().is_some() && T::MembershipChanged::get_prime().is_some());
			#[cfg(test)] crate::tests::clean();
		}

		// er keep the prime common between incoming and outgoing to make sure it is rejigged.
		reset_member {
			let m in 1 .. T::MaxMembers::get();

			let members = (1..m+1).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			set_members::<T, I>(members.clone(), Some(members.len() - 1));
			let mut new_members = (m..2*m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
		}: {
			assert_ok!(<Membership<T, _>>::reset_members(T::ResetOrigin::successful_origin(), new_members.clone()));
		} verify {
			new_members.sort();
			assert_eq!(<Members<T, _>>::get(), new_members);
			// prime is rejigged
			assert!(<Prime<T, _>>::get().is_some() && T::MembershipChanged::get_prime().is_some());
			#[cfg(test)] crate::tests::clean();
		}

		change_key {
			let m in 1 .. T::MaxMembers::get();

			// worse case would be to change the prime
			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			let prime = members.last().cloned().unwrap();
			set_members::<T, I>(members.clone(), Some(members.len() - 1));

			let add = account::<T::AccountId>("member", m, SEED);
			whitelist!(prime);
		}: {
			assert_ok!(<Membership<T, _>>::change_key(RawOrigin::Signed(prime.clone()).into(), add.clone()));
		} verify {
			assert!(!<Members<T, _>>::get().contains(&prime));
			assert!(<Members<T, _>>::get().contains(&add));
			// prime is rejigged
			assert_eq!(<Prime<T, _>>::get().unwrap(), add);
			#[cfg(test)] crate::tests::clean();
		}

		set_prime {
			let m in 1 .. T::MaxMembers::get();
			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			let prime = members.last().cloned().unwrap();
			set_members::<T, I>(members, None);
		}: {
			assert_ok!(<Membership<T, _>>::set_prime(T::PrimeOrigin::successful_origin(), prime));
		} verify {
			assert!(<Prime<T, _>>::get().is_some());
			assert!(<T::MembershipChanged>::get_prime().is_some());
			#[cfg(test)] crate::tests::clean();
		}

		clear_prime {
			let m in 1 .. T::MaxMembers::get();
			let members = (0..m).map(|i| account("member", i, SEED)).collect::<Vec<T::AccountId>>();
			let prime = members.last().cloned().unwrap();
			set_members::<T, I>(members, None);
		}: {
			assert_ok!(<Membership<T, _>>::clear_prime(T::PrimeOrigin::successful_origin()));
		} verify {
			assert!(<Prime<T, _>>::get().is_none());
			assert!(<T::MembershipChanged>::get_prime().is_none());
			#[cfg(test)] crate::tests::clean();
		}
	}

	impl_benchmark_test_suite!(Membership, crate::tests::new_bench_ext(), crate::tests::Test,);
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate as pallet_membership;

	use frame_support::{assert_ok, assert_noop, parameter_types, ord_parameter_types};
	use sp_core::H256;
	use sp_runtime::{traits::{BlakeTwo256, IdentityLookup, BadOrigin}, testing::Header};
	use frame_system::EnsureSignedBy;

	type UncheckedExtrinsic = frame_system::mocking::MockUncheckedExtrinsic<Test>;
	type Block = frame_system::mocking::MockBlock<Test>;

	frame_support::construct_runtime!(
		pub enum Test where
			Block = Block,
			NodeBlock = Block,
			UncheckedExtrinsic = UncheckedExtrinsic,
		{
			System: frame_system::{Pallet, Call, Config, Storage, Event<T>},
			Membership: pallet_membership::{Pallet, Call, Storage, Config<T>, Event<T>},
		}
	);

	parameter_types! {
		pub const BlockHashCount: u64 = 250;
		pub const MaxMembers: u32 = 10;
		pub BlockWeights: frame_system::limits::BlockWeights =
			frame_system::limits::BlockWeights::simple_max(1024);
		pub static Members: Vec<u64> = vec![];
		pub static Prime: Option<u64> = None;
	}

	impl frame_system::Config for Test {
		type BaseCallFilter = ();
		type BlockWeights = ();
		type BlockLength = ();
		type DbWeight = ();
		type Origin = Origin;
		type Index = u64;
		type BlockNumber = u64;
		type Hash = H256;
		type Call = Call;
		type Hashing = BlakeTwo256;
		type AccountId = u64;
		type Lookup = IdentityLookup<Self::AccountId>;
		type Header = Header;
		type Event = Event;
		type BlockHashCount = BlockHashCount;
		type Version = ();
		type PalletInfo = PalletInfo;
		type AccountData = ();
		type OnNewAccount = ();
		type OnKilledAccount = ();
		type SystemWeightInfo = ();
		type SS58Prefix = ();
		type OnSetCode = ();
	}
	ord_parameter_types! {
		pub const One: u64 = 1;
		pub const Two: u64 = 2;
		pub const Three: u64 = 3;
		pub const Four: u64 = 4;
		pub const Five: u64 = 5;
	}

	pub struct TestChangeMembers;
	impl ChangeMembers<u64> for TestChangeMembers {
		fn change_members_sorted(incoming: &[u64], outgoing: &[u64], new: &[u64]) {
			let mut old_plus_incoming = Members::get();
			old_plus_incoming.extend_from_slice(incoming);
			old_plus_incoming.sort();
			let mut new_plus_outgoing = new.to_vec();
			new_plus_outgoing.extend_from_slice(outgoing);
			new_plus_outgoing.sort();
			assert_eq!(old_plus_incoming, new_plus_outgoing);

			Members::set(new.to_vec());
			Prime::set(None);
		}
		fn set_prime(who: Option<u64>) {
			Prime::set(who);
		}
		fn get_prime() -> Option<u64> {
			Prime::get()
		}
	}

	impl InitializeMembers<u64> for TestChangeMembers {
		fn initialize_members(members: &[u64]) {
			MEMBERS.with(|m| *m.borrow_mut() = members.to_vec());
		}
	}

	impl Config for Test {
		type Event = Event;
		type AddOrigin = EnsureSignedBy<One, u64>;
		type RemoveOrigin = EnsureSignedBy<Two, u64>;
		type SwapOrigin = EnsureSignedBy<Three, u64>;
		type ResetOrigin = EnsureSignedBy<Four, u64>;
		type PrimeOrigin = EnsureSignedBy<Five, u64>;
		type MembershipInitialized = TestChangeMembers;
		type MembershipChanged = TestChangeMembers;
		type MaxMembers = MaxMembers;
		type WeightInfo = ();
	}

	pub(crate) fn new_test_ext() -> sp_io::TestExternalities {
		let mut t = frame_system::GenesisConfig::default().build_storage::<Test>().unwrap();
		// We use default for brevity, but you can configure as desired if needed.
		pallet_membership::GenesisConfig::<Test>{
			members: vec![10, 20, 30],
			.. Default::default()
		}.assimilate_storage(&mut t).unwrap();
		t.into()
	}

	#[cfg(feature = "runtime-benchmarks")]
	pub(crate) fn new_bench_ext() -> sp_io::TestExternalities {
		frame_system::GenesisConfig::default().build_storage::<Test>().unwrap().into()
	}

	#[cfg(feature = "runtime-benchmarks")]
	pub(crate) fn clean() {
		Members::set(vec![]);
		Prime::set(None);
	}

	#[test]
	fn query_membership_works() {
		new_test_ext().execute_with(|| {
			assert_eq!(Membership::members(), vec![10, 20, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), vec![10, 20, 30]);
		});
	}

	#[test]
	fn prime_member_works() {
		new_test_ext().execute_with(|| {
			assert_noop!(Membership::set_prime(Origin::signed(4), 20), BadOrigin);
			assert_noop!(Membership::set_prime(Origin::signed(5), 15), Error::<Test, _>::NotMember);
			assert_ok!(Membership::set_prime(Origin::signed(5), 20));
			assert_eq!(Membership::prime(), Some(20));
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());

			assert_ok!(Membership::clear_prime(Origin::signed(5)));
			assert_eq!(Membership::prime(), None);
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());
		});
	}

	#[test]
	fn add_member_works() {
		new_test_ext().execute_with(|| {
			assert_noop!(Membership::add_member(Origin::signed(5), 15), BadOrigin);
			assert_noop!(Membership::add_member(Origin::signed(1), 10), Error::<Test, _>::AlreadyMember);
			assert_ok!(Membership::add_member(Origin::signed(1), 15));
			assert_eq!(Membership::members(), vec![10, 15, 20, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
		});
	}

	#[test]
	fn remove_member_works() {
		new_test_ext().execute_with(|| {
			assert_noop!(Membership::remove_member(Origin::signed(5), 20), BadOrigin);
			assert_noop!(Membership::remove_member(Origin::signed(2), 15), Error::<Test, _>::NotMember);
			assert_ok!(Membership::set_prime(Origin::signed(5), 20));
			assert_ok!(Membership::remove_member(Origin::signed(2), 20));
			assert_eq!(Membership::members(), vec![10, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
			assert_eq!(Membership::prime(), None);
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());
		});
	}

	#[test]
	fn swap_member_works() {
		new_test_ext().execute_with(|| {
			assert_noop!(Membership::swap_member(Origin::signed(5), 10, 25), BadOrigin);
			assert_noop!(Membership::swap_member(Origin::signed(3), 15, 25), Error::<Test, _>::NotMember);
			assert_noop!(Membership::swap_member(Origin::signed(3), 10, 30), Error::<Test, _>::AlreadyMember);

			assert_ok!(Membership::set_prime(Origin::signed(5), 20));
			assert_ok!(Membership::swap_member(Origin::signed(3), 20, 20));
			assert_eq!(Membership::members(), vec![10, 20, 30]);
			assert_eq!(Membership::prime(), Some(20));
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());

			assert_ok!(Membership::set_prime(Origin::signed(5), 10));
			assert_ok!(Membership::swap_member(Origin::signed(3), 10, 25));
			assert_eq!(Membership::members(), vec![20, 25, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
			assert_eq!(Membership::prime(), None);
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());
		});
	}

	#[test]
	fn swap_member_works_that_does_not_change_order() {
		new_test_ext().execute_with(|| {
			assert_ok!(Membership::swap_member(Origin::signed(3), 10, 5));
			assert_eq!(Membership::members(), vec![5, 20, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
		});
	}

	#[test]
	fn change_key_works() {
		new_test_ext().execute_with(|| {
			assert_ok!(Membership::set_prime(Origin::signed(5), 10));
			assert_noop!(Membership::change_key(Origin::signed(3), 25), Error::<Test, _>::NotMember);
			assert_noop!(Membership::change_key(Origin::signed(10), 20), Error::<Test, _>::AlreadyMember);
			assert_ok!(Membership::change_key(Origin::signed(10), 40));
			assert_eq!(Membership::members(), vec![20, 30, 40]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
			assert_eq!(Membership::prime(), Some(40));
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());
		});
	}

	#[test]
	fn change_key_works_that_does_not_change_order() {
		new_test_ext().execute_with(|| {
			assert_ok!(Membership::change_key(Origin::signed(10), 5));
			assert_eq!(Membership::members(), vec![5, 20, 30]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
		});
	}

	#[test]
	fn reset_members_works() {
		new_test_ext().execute_with(|| {
			assert_ok!(Membership::set_prime(Origin::signed(5), 20));
			assert_noop!(Membership::reset_members(Origin::signed(1), vec![20, 40, 30]), BadOrigin);

			assert_ok!(Membership::reset_members(Origin::signed(4), vec![20, 40, 30]));
			assert_eq!(Membership::members(), vec![20, 30, 40]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
			assert_eq!(Membership::prime(), Some(20));
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());

			assert_ok!(Membership::reset_members(Origin::signed(4), vec![10, 40, 30]));
			assert_eq!(Membership::members(), vec![10, 30, 40]);
			assert_eq!(MEMBERS.with(|m| m.borrow().clone()), Membership::members());
			assert_eq!(Membership::prime(), None);
			assert_eq!(PRIME.with(|m| *m.borrow()), Membership::prime());
		});
	}

	#[test]
	#[should_panic(expected = "Members cannot contain duplicate accounts.")]
	fn genesis_build_panics_with_duplicate_members() {
		pallet_membership::GenesisConfig::<Test> {
			members: vec![1, 2, 3, 1],
			phantom: Default::default(),
		}.build_storage().unwrap();
	}
}
