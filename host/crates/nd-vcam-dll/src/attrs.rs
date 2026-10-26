// SPDX-License-Identifier: GPL-3.0-or-later
//! `IMFAttributes` boilerplate: every method forwards to an inner store created with
//! `MFCreateAttributes`. Used by the source (as `IMFActivate`) and the stream.

macro_rules! forward_attributes {
    ($ty:ident) => {
        impl ::windows::Win32::Media::MediaFoundation::IMFAttributes_Impl for $ty {
            fn GetItem(
                &self,
                k: *const ::windows::core::GUID,
                v: *mut ::windows::core::PROPVARIANT,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.GetItem(k, ::core::option::Option::Some(v)) }
            }
            fn GetItemType(
                &self,
                k: *const ::windows::core::GUID,
            ) -> ::windows::core::Result<::windows::Win32::Media::MediaFoundation::MF_ATTRIBUTE_TYPE> {
                unsafe { self.attrs.GetItemType(k) }
            }
            fn CompareItem(
                &self,
                k: *const ::windows::core::GUID,
                v: *const ::windows::core::PROPVARIANT,
            ) -> ::windows::core::Result<::windows::Win32::Foundation::BOOL> {
                unsafe { self.attrs.CompareItem(k, v) }
            }
            fn Compare(
                &self,
                t: ::core::option::Option<&::windows::Win32::Media::MediaFoundation::IMFAttributes>,
                m: ::windows::Win32::Media::MediaFoundation::MF_ATTRIBUTES_MATCH_TYPE,
            ) -> ::windows::core::Result<::windows::Win32::Foundation::BOOL> {
                unsafe { self.attrs.Compare(t, m) }
            }
            fn GetUINT32(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<u32> {
                unsafe { self.attrs.GetUINT32(k) }
            }
            fn GetUINT64(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<u64> {
                unsafe { self.attrs.GetUINT64(k) }
            }
            fn GetDouble(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<f64> {
                unsafe { self.attrs.GetDouble(k) }
            }
            fn GetGUID(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<::windows::core::GUID> {
                unsafe { self.attrs.GetGUID(k) }
            }
            fn GetStringLength(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<u32> {
                unsafe { self.attrs.GetStringLength(k) }
            }
            fn GetString(
                &self,
                k: *const ::windows::core::GUID,
                buf: ::windows::core::PWSTR,
                cch: u32,
                out_len: *mut u32,
            ) -> ::windows::core::Result<()> {
                unsafe {
                    self.attrs.GetString(
                        k,
                        ::core::slice::from_raw_parts_mut(buf.0, cch as usize),
                        ::core::option::Option::Some(out_len),
                    )
                }
            }
            fn GetAllocatedString(
                &self,
                k: *const ::windows::core::GUID,
                out: *mut ::windows::core::PWSTR,
                out_len: *mut u32,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.GetAllocatedString(k, out, out_len) }
            }
            fn GetBlobSize(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<u32> {
                unsafe { self.attrs.GetBlobSize(k) }
            }
            fn GetBlob(
                &self,
                k: *const ::windows::core::GUID,
                buf: *mut u8,
                cb: u32,
                out_len: *mut u32,
            ) -> ::windows::core::Result<()> {
                unsafe {
                    self.attrs.GetBlob(
                        k,
                        ::core::slice::from_raw_parts_mut(buf, cb as usize),
                        ::core::option::Option::Some(out_len),
                    )
                }
            }
            fn GetAllocatedBlob(
                &self,
                k: *const ::windows::core::GUID,
                buf: *mut *mut u8,
                size: *mut u32,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.GetAllocatedBlob(k, buf, size) }
            }
            fn GetUnknown(
                &self,
                k: *const ::windows::core::GUID,
                riid: *const ::windows::core::GUID,
                ppv: *mut *mut ::core::ffi::c_void,
            ) -> ::windows::core::Result<()> {
                unsafe {
                    use ::windows::core::Interface;
                    (Interface::vtable(&self.attrs).GetUnknown)(Interface::as_raw(&self.attrs), k, riid, ppv).ok()
                }
            }
            fn SetItem(
                &self,
                k: *const ::windows::core::GUID,
                v: *const ::windows::core::PROPVARIANT,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetItem(k, v) }
            }
            fn DeleteItem(&self, k: *const ::windows::core::GUID) -> ::windows::core::Result<()> {
                unsafe { self.attrs.DeleteItem(k) }
            }
            fn DeleteAllItems(&self) -> ::windows::core::Result<()> {
                unsafe { self.attrs.DeleteAllItems() }
            }
            fn SetUINT32(&self, k: *const ::windows::core::GUID, v: u32) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetUINT32(k, v) }
            }
            fn SetUINT64(&self, k: *const ::windows::core::GUID, v: u64) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetUINT64(k, v) }
            }
            fn SetDouble(&self, k: *const ::windows::core::GUID, v: f64) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetDouble(k, v) }
            }
            fn SetGUID(
                &self,
                k: *const ::windows::core::GUID,
                v: *const ::windows::core::GUID,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetGUID(k, v) }
            }
            fn SetString(
                &self,
                k: *const ::windows::core::GUID,
                v: &::windows::core::PCWSTR,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetString(k, *v) }
            }
            fn SetBlob(&self, k: *const ::windows::core::GUID, buf: *const u8, cb: u32) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetBlob(k, ::core::slice::from_raw_parts(buf, cb as usize)) }
            }
            fn SetUnknown(
                &self,
                k: *const ::windows::core::GUID,
                v: ::core::option::Option<&::windows::core::IUnknown>,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.SetUnknown(k, v) }
            }
            fn LockStore(&self) -> ::windows::core::Result<()> {
                unsafe { self.attrs.LockStore() }
            }
            fn UnlockStore(&self) -> ::windows::core::Result<()> {
                unsafe { self.attrs.UnlockStore() }
            }
            fn GetCount(&self) -> ::windows::core::Result<u32> {
                unsafe { self.attrs.GetCount() }
            }
            fn GetItemByIndex(
                &self,
                i: u32,
                k: *mut ::windows::core::GUID,
                v: *mut ::windows::core::PROPVARIANT,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.GetItemByIndex(i, k, ::core::option::Option::Some(v)) }
            }
            fn CopyAllItems(
                &self,
                dest: ::core::option::Option<&::windows::Win32::Media::MediaFoundation::IMFAttributes>,
            ) -> ::windows::core::Result<()> {
                unsafe { self.attrs.CopyAllItems(dest) }
            }
        }
    };
}
pub(crate) use forward_attributes;
