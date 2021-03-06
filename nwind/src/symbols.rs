use std::slice;
use std::str;
use std::ops::{Range, Index};
use std::sync::Arc;
use std::mem::{self, ManuallyDrop};
use std::time::Instant;

use range_map::RangeMap;
use elf::{self, Strtab, Endian};
use utils::{StableIndex, get_ms};
use types::{Bitness, Endianness};
use binary::{BinaryData, SymbolTable};

pub struct Symbols< T > {
    strtab_owner: ManuallyDrop< Arc< T > >,
    symbols: ManuallyDrop< RangeMap< &'static str > >
}

impl< T > Drop for Symbols< T > {
    #[inline]
    fn drop( &mut self ) {
        unsafe {
            ManuallyDrop::drop( &mut self.symbols );
            ManuallyDrop::drop( &mut self.strtab_owner );
        }
    }
}

fn load_symbols< 'a >( architecture: &str, bitness: Bitness, endianness: Endianness, sym_bytes: &[u8], strtab_bytes: &'a [u8], symbols: &mut Vec< (Range< u64 >, &'a str) > ) {
    macro_rules! select_branch {
        (if ($condition: expr) { $true_case:expr } else { $false_case:expr } => |$name:ident| $callback:expr) => {
            if $condition {
                let $name = $true_case;
                $callback
            } else {
                let $name = $false_case;
                $callback
            }
        }
    }

    let is_arm = architecture == "arm";
    let endian = match endianness {
        Endianness::LittleEndian => Endian::Little,
        Endianness::BigEndian => Endian::Big
    };

    let strtab = Strtab::new( strtab_bytes, 0x0 );
    select_branch! {
        if (bitness == Bitness::B64) {
            elf::Elf64SymIter::new( sym_bytes, endian )
        } else {
            elf::Elf32SymIter::new( sym_bytes, endian )
        } => |syms| {
            for sym in syms {
                if !sym.is_function() || sym.st_size == 0 || sym.st_value == 0 {
                    continue;
                }
                if let Some( Ok( name ) ) = strtab.get( sym.st_name ) {
                    let mut start = sym.st_value as u64;
                    if is_arm {
                        // On ARM the lowest bit of the symbol value specifies
                        // whenever the instruction it points at is an ARM or
                        // a Thumb one, so we mask it out.
                        // Source: ELF for the ARM Architecture
                        //         http://infocenter.arm.com/help/topic/com.arm.doc.ihi0044f/IHI0044F_aaelf.pdf
                        start = start & !1;
                    }

                    let end = start + sym.st_size as u64;
                    symbols.push( ((start..end), name) );
                }
            }
        }
    }
}

impl Symbols< BinaryData > {
    pub fn load_from_binary_data( data: &Arc< BinaryData > ) -> Self {
        Symbols::load(
            &data.name(),
            data.architecture(),
            data.bitness(),
            data.endianness(),
            data.symbol_tables(),
            &**data,
            data
        )
    }
}

impl< T: StableIndex + Index< Range< u64 >, Output = [u8] > > Symbols< T > {
    pub fn load< S >(
        name: &str,
        architecture: &str,
        bitness: Bitness,
        endianness: Endianness,
        symbol_tables: &[SymbolTable],
        symbol_tables_bytes: &S,
        strtab_owner: &Arc< T >
    ) -> Self where S: ?Sized + Index< Range< u64 >, Output = [u8] > {
        let start_timestamp = Instant::now();

        let mut symbols: Vec< (Range< u64 >, &str) > = Vec::new();
        let mut normal_count = 0;
        let mut dynamic_count = 0;

        for symbol_table in symbol_tables {
            let sym_bytes = &symbol_tables_bytes[ symbol_table.range.clone() ];
            let strtab_bytes = &strtab_owner[ symbol_table.strtab_range.clone() ];

            let count_before = symbols.len();
            load_symbols( architecture, bitness, endianness, sym_bytes, strtab_bytes, &mut symbols );

            let count = symbols.len() - count_before;
            if symbol_table.is_dynamic {
                dynamic_count += count;
            } else {
                normal_count += count;
            }
        }

        let symbols: Vec< (Range< u64 >, &'static str) > = unsafe { mem::transmute( symbols ) };
        let elapsed = start_timestamp.elapsed();
        debug!( "Loaded {} symbols for '{}' ({} normal, {} dynamic) in {}ms", symbols.len(), name, normal_count, dynamic_count, get_ms( elapsed ) );
        Symbols {
            strtab_owner: ManuallyDrop::new( strtab_owner.clone() ),
            symbols: ManuallyDrop::new( RangeMap::from_vec( symbols ) )
        }
    }

    #[inline]
    fn as_range_map< 'a >( &'a self ) -> &'a RangeMap< &'a str > {
        &self.symbols
    }

    #[inline]
    pub fn get_symbol_index( &self, address: u64 ) -> Option< usize > {
        self.as_range_map().get_index( address )
    }

    #[inline]
    pub fn get_symbol_by_index( &self, index: usize ) -> Option< (Range< u64 >, &str) > {
        self.as_range_map().get_by_index( index ).map( |(range, name)| (range, *name) )
    }

    #[inline]
    pub fn get_symbol_with_lifetime_by_index< 'a >( &self, strtab_owner: &'a T, index: usize ) -> Option< (Range< u64 >, &'a str) > {
        if strtab_owner as *const T == &*self.strtab_owner as &T as *const T {
            self.get_symbol_by_index( index ).map( |(range, name)| {
                let name = unsafe {
                    str::from_utf8_unchecked( slice::from_raw_parts( name.as_ptr(), name.len() ) )
                };
                (range, name)
            })
        } else {
            None
        }
    }
}
