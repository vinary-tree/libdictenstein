module vinary_tree_libdictenstein
  use, intrinsic :: iso_c_binding
  use vinary_tree_interop, only: vt_resource, vt_unit_byte, vt_unit_unicode_scalar, vt_unit_u64
  implicit none
  private

  integer(c_int32_t), parameter, public :: ldict_ok = 0
  integer(c_int32_t), parameter, public :: dynamic_dawg_kind = 1, double_array_trie_kind = 2
  integer(c_int32_t), parameter, public :: scdawg_kind = 3, persistent_artrie_kind = 4
  integer(c_int32_t), parameter, public :: persistent_vocabulary_kind = 5
  integer(c_int64_t), parameter, public :: can_read = 1, can_insert = 2, can_remove = 4
  integer(c_int64_t), parameter, public :: can_clear = 8, can_compact = 16
  integer(c_int64_t), parameter, public :: can_substring = 32, can_checkpoint = 64
  public :: vt_unit_byte, vt_unit_unicode_scalar, vt_unit_u64

  type, bind(c) :: optional_u64
    integer(c_int64_t) :: value
    integer(c_int8_t) :: has_value
    integer(c_int8_t) :: reserved(7)
  end type
  type, bind(c) :: text_entry
    type(c_ptr) :: data
    integer(c_size_t) :: len
    type(optional_u64) :: value
  end type

  type, public :: dictionary
    private
    type(c_ptr) :: handle = c_null_ptr
  contains
    procedure :: resource => dictionary_resource
    procedure :: kind => dictionary_kind
    procedure :: capabilities => dictionary_capabilities
    procedure :: length => dictionary_length
    procedure :: put_text, remove_text, contains_text, get_text
    procedure :: put_u64, remove_u64, contains_u64, get_u64
    procedure :: put_all
    procedure :: clear => dictionary_clear
    procedure :: compact => dictionary_compact
    procedure :: checkpoint => dictionary_checkpoint
    procedure :: contains_substring, substring_frequency, vocabulary_term
    procedure :: close => dictionary_close
    final :: dictionary_finalize
  end type

  public :: new_dynamic_dawg, new_double_array_trie, new_scdawg
  public :: create_persistent_artrie, open_persistent_artrie
  public :: create_persistent_vocabulary, open_persistent_vocabulary

  interface
    function c_dynamic(domain, output) bind(c, name="ldict_dynamic_dawg_new") result(status)
      import c_int32_t, c_ptr
      integer(c_int32_t), value :: domain; type(c_ptr), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_dat(domain, entries, count, output) bind(c, name="ldict_double_array_trie_new") result(status)
      import c_int32_t, c_ptr, c_size_t
      integer(c_int32_t), value :: domain; type(c_ptr), value :: entries
      integer(c_size_t), value :: count; type(c_ptr), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_scdawg(domain, output) bind(c, name="ldict_scdawg_new") result(status)
      import c_int32_t, c_ptr
      integer(c_int32_t), value :: domain; type(c_ptr), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_pcreate(domain, path, count, output) bind(c, name="ldict_persistent_artrie_create") result(status)
      import c_int32_t, c_ptr, c_size_t
      integer(c_int32_t), value :: domain; type(c_ptr), value :: path; integer(c_size_t), value :: count
      type(c_ptr), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_popen(domain, path, count, output) bind(c, name="ldict_persistent_artrie_open") result(status)
      import c_int32_t, c_ptr, c_size_t
      integer(c_int32_t), value :: domain; type(c_ptr), value :: path; integer(c_size_t), value :: count
      type(c_ptr), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_vcreate(path, count, output) bind(c, name="ldict_persistent_vocab_create") result(status)
      import c_int32_t, c_ptr, c_size_t
      type(c_ptr), value :: path; integer(c_size_t), value :: count; type(c_ptr), intent(out) :: output
      integer(c_int32_t) :: status
    end function
    function c_vopen(path, count, output) bind(c, name="ldict_persistent_vocab_open") result(status)
      import c_int32_t, c_ptr, c_size_t
      type(c_ptr), value :: path; integer(c_size_t), value :: count; type(c_ptr), intent(out) :: output
      integer(c_int32_t) :: status
    end function
    subroutine c_free(handle) bind(c, name="ldict_dictionary_free")
      import c_ptr; type(c_ptr), value :: handle
    end subroutine
    function c_resource(handle, output) bind(c, name="ldict_dictionary_resource") result(status)
      import c_ptr, c_int32_t, vt_resource
      type(c_ptr), value :: handle; type(vt_resource), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_kind(handle, output) bind(c, name="ldict_dictionary_kind") result(status)
      import c_ptr, c_int32_t
      type(c_ptr), value :: handle; integer(c_int32_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_caps(handle, output) bind(c, name="ldict_dictionary_capabilities") result(status)
      import c_ptr, c_int32_t, c_int64_t
      type(c_ptr), value :: handle; integer(c_int64_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_len(handle, output) bind(c, name="ldict_dictionary_len") result(status)
      import c_ptr, c_int32_t, c_size_t
      type(c_ptr), value :: handle; integer(c_size_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_put_text(handle, data, count, value, has_value, output) &
        bind(c, name="ldict_dictionary_insert_text_value") result(status)
      import c_ptr, c_int32_t, c_int64_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count; integer(c_int64_t), value :: value
      integer(c_int8_t), value :: has_value; integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_remove_text(handle, data, count, output) bind(c, name="ldict_dictionary_remove_text") result(status)
      import c_ptr, c_int32_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_contains_text(handle, data, count, output) bind(c, name="ldict_dictionary_contains_text") result(status)
      import c_ptr, c_int32_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_get_text(handle, data, count, found, value, has_value) bind(c, name="ldict_dictionary_get_text_value") result(status)
      import c_ptr, c_int32_t, c_int64_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: found; integer(c_int64_t), intent(out) :: value
      integer(c_int8_t), intent(out) :: has_value; integer(c_int32_t) :: status
    end function
    function c_put_u64(handle, data, count, value, has_value, output) &
        bind(c, name="ldict_dictionary_insert_u64_value") result(status)
      import c_ptr, c_int32_t, c_int64_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count; integer(c_int64_t), value :: value
      integer(c_int8_t), value :: has_value; integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_remove_u64(handle, data, count, output) bind(c, name="ldict_dictionary_remove_u64") result(status)
      import c_ptr, c_int32_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_contains_u64(handle, data, count, output) bind(c, name="ldict_dictionary_contains_u64") result(status)
      import c_ptr, c_int32_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_get_u64(handle, data, count, found, value, has_value) bind(c, name="ldict_dictionary_get_u64_value") result(status)
      import c_ptr, c_int32_t, c_int64_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: found; integer(c_int64_t), intent(out) :: value
      integer(c_int8_t), intent(out) :: has_value; integer(c_int32_t) :: status
    end function
    function c_batch(handle, entries, count, output) bind(c, name="ldict_dictionary_insert_text_batch") result(status)
      import c_ptr, c_int32_t, c_size_t
      type(c_ptr), value :: handle, entries; integer(c_size_t), value :: count
      integer(c_size_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_clear(handle) bind(c, name="ldict_dictionary_clear") result(status)
      import c_ptr, c_int32_t; type(c_ptr), value :: handle; integer(c_int32_t) :: status
    end function
    function c_compact(handle, output) bind(c, name="ldict_dictionary_compact") result(status)
      import c_ptr, c_int32_t, c_size_t
      type(c_ptr), value :: handle; integer(c_size_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_checkpoint(handle) bind(c, name="ldict_dictionary_checkpoint") result(status)
      import c_ptr, c_int32_t; type(c_ptr), value :: handle; integer(c_int32_t) :: status
    end function
    function c_substring(handle, data, count, output) bind(c, name="ldict_scdawg_contains_substring") result(status)
      import c_ptr, c_int32_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_int8_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_frequency(handle, data, count, output) bind(c, name="ldict_scdawg_substring_frequency") result(status)
      import c_ptr, c_int32_t, c_size_t
      type(c_ptr), value :: handle, data; integer(c_size_t), value :: count
      integer(c_size_t), intent(out) :: output; integer(c_int32_t) :: status
    end function
    function c_vocab_term(handle, index, output, capacity, count, found) bind(c, name="ldict_vocab_get_term") result(status)
      import c_ptr, c_int32_t, c_int64_t, c_int8_t, c_size_t
      type(c_ptr), value :: handle, output; integer(c_int64_t), value :: index
      integer(c_size_t), value :: capacity; integer(c_size_t), intent(out) :: count
      integer(c_int8_t), intent(out) :: found; integer(c_int32_t) :: status
    end function
  end interface

contains

  function string_bytes(text) result(bytes)
    character(len=*), intent(in) :: text
    character(kind=c_char), allocatable, target :: bytes(:)
    integer :: i
    allocate(bytes(len(text))); do i = 1, len(text); bytes(i) = text(i:i); end do
  end function
  function chars_ptr(bytes) result(pointer)
    character(kind=c_char), target, intent(in) :: bytes(:); type(c_ptr) :: pointer
    if (size(bytes) == 0) then; pointer = c_null_ptr; else; pointer = c_loc(bytes(1)); end if
  end function
  function tokens_ptr(values) result(pointer)
    integer(c_int64_t), target, intent(in) :: values(:); type(c_ptr) :: pointer
    if (size(values) == 0) then; pointer = c_null_ptr; else; pointer = c_loc(values(1)); end if
  end function

  subroutine new_dynamic_dawg(value, domain, status)
    type(dictionary), intent(out) :: value; integer(c_int32_t), intent(in), optional :: domain
    integer(c_int32_t), intent(out), optional :: status; integer(c_int32_t) :: code, selected
    selected = vt_unit_unicode_scalar; if (present(domain)) selected = domain
    code = c_dynamic(selected, value%handle); if (present(status)) status = code
  end subroutine

  subroutine make_entries(terms, values, has_values, entries, arena)
    character(len=*), intent(in) :: terms(:); integer(c_int64_t), intent(in), optional :: values(:)
    logical, intent(in), optional :: has_values(:); type(text_entry), allocatable, target, intent(out) :: entries(:)
    character(kind=c_char), allocatable, target, intent(out) :: arena(:)
    integer :: total, item, offset, count, i
    total = 0; do item = 1, size(terms); total = total + len_trim(terms(item)); end do
    allocate(entries(size(terms)), arena(total)); offset = 1
    do item = 1, size(terms)
      count = len_trim(terms(item))
      do i = 1, count; arena(offset+i-1) = terms(item)(i:i); end do
      if (count == 0) then; entries(item)%data = c_null_ptr; else; entries(item)%data = c_loc(arena(offset)); end if
      entries(item)%len = int(count, c_size_t); entries(item)%value%value = 0
      entries(item)%value%has_value = 0; entries(item)%value%reserved = 0
      if (present(values)) entries(item)%value%value = values(item)
      if (present(has_values)) then
        if (has_values(item)) entries(item)%value%has_value = 1
      else if (present(values)) then
        entries(item)%value%has_value = 1
      end if
      offset = offset + count
    end do
  end subroutine

  subroutine new_double_array_trie(value, terms, values, has_values, domain, status)
    type(dictionary), intent(out) :: value; character(len=*), intent(in) :: terms(:)
    integer(c_int64_t), intent(in), optional :: values(:); logical, intent(in), optional :: has_values(:)
    integer(c_int32_t), intent(in), optional :: domain; integer(c_int32_t), intent(out), optional :: status
    type(text_entry), allocatable, target :: entries(:); character(kind=c_char), allocatable, target :: arena(:)
    integer(c_int32_t) :: code, selected; type(c_ptr) :: pointer
    selected = vt_unit_unicode_scalar; if (present(domain)) selected = domain
    call make_entries(terms, values, has_values, entries, arena)
    if (size(entries) == 0) then; pointer = c_null_ptr; else; pointer = c_loc(entries(1)); end if
    code = c_dat(selected, pointer, int(size(entries), c_size_t), value%handle); if (present(status)) status = code
  end subroutine
  subroutine new_scdawg(value, domain, status)
    type(dictionary), intent(out) :: value; integer(c_int32_t), intent(in), optional :: domain
    integer(c_int32_t), intent(out), optional :: status; integer(c_int32_t) :: code, selected
    selected = vt_unit_unicode_scalar; if (present(domain)) selected = domain
    code = c_scdawg(selected, value%handle); if (present(status)) status = code
  end subroutine

  subroutine open_backend(value, path, domain, create, vocabulary, status)
    type(dictionary), intent(out) :: value; character(len=*), intent(in) :: path
    integer(c_int32_t), intent(in) :: domain; logical, intent(in) :: create, vocabulary
    integer(c_int32_t), intent(out), optional :: status
    character(kind=c_char), allocatable, target :: bytes(:); integer(c_int32_t) :: code
    bytes = string_bytes(path)
    if (vocabulary) then
      if (create) then; code = c_vcreate(chars_ptr(bytes), int(size(bytes),c_size_t), value%handle)
      else; code = c_vopen(chars_ptr(bytes), int(size(bytes),c_size_t), value%handle); end if
    else
      if (create) then; code = c_pcreate(domain, chars_ptr(bytes), int(size(bytes),c_size_t), value%handle)
      else; code = c_popen(domain, chars_ptr(bytes), int(size(bytes),c_size_t), value%handle); end if
    end if
    if (present(status)) status = code
  end subroutine
  subroutine create_persistent_artrie(value,path,domain,status)
    type(dictionary),intent(out)::value; character(len=*),intent(in)::path; integer(c_int32_t),intent(in),optional::domain
    integer(c_int32_t),intent(out),optional::status; integer(c_int32_t)::d
    d=vt_unit_unicode_scalar; if(present(domain))d=domain; call open_backend(value,path,d,.true.,.false.,status)
  end subroutine
  subroutine open_persistent_artrie(value,path,domain,status)
    type(dictionary),intent(out)::value; character(len=*),intent(in)::path; integer(c_int32_t),intent(in),optional::domain
    integer(c_int32_t),intent(out),optional::status; integer(c_int32_t)::d
    d=vt_unit_unicode_scalar; if(present(domain))d=domain; call open_backend(value,path,d,.false.,.false.,status)
  end subroutine
  subroutine create_persistent_vocabulary(value,path,status)
    type(dictionary),intent(out)::value; character(len=*),intent(in)::path; integer(c_int32_t),intent(out),optional::status
    call open_backend(value,path,vt_unit_unicode_scalar,.true.,.true.,status)
  end subroutine
  subroutine open_persistent_vocabulary(value,path,status)
    type(dictionary),intent(out)::value; character(len=*),intent(in)::path; integer(c_int32_t),intent(out),optional::status
    call open_backend(value,path,vt_unit_unicode_scalar,.false.,.true.,status)
  end subroutine

  subroutine dictionary_resource(self,value,status)
    class(dictionary),intent(in)::self; type(vt_resource),intent(out)::value; integer(c_int32_t),intent(out),optional::status
    integer(c_int32_t)::code; code=c_resource(self%handle,value); if(present(status))status=code
  end subroutine
  function dictionary_kind(self,status) result(value)
    class(dictionary),intent(in)::self; integer(c_int32_t),intent(out),optional::status; integer(c_int32_t)::value,code
    code=c_kind(self%handle,value); if(present(status))status=code
  end function
  function dictionary_capabilities(self,status) result(value)
    class(dictionary),intent(in)::self; integer(c_int32_t),intent(out),optional::status; integer(c_int64_t)::value
    integer(c_int32_t)::code; code=c_caps(self%handle,value); if(present(status))status=code
  end function
  function dictionary_length(self,status) result(value)
    class(dictionary),intent(in)::self; integer(c_int32_t),intent(out),optional::status; integer(c_size_t)::value
    integer(c_int32_t)::code; code=c_len(self%handle,value); if(present(status))status=code
  end function

  subroutine put_text(self,term,value,has_value,inserted,status)
    class(dictionary),intent(inout)::self; character(len=*),intent(in)::term; integer(c_int64_t),intent(in),optional::value
    logical,intent(in),optional::has_value; logical,intent(out)::inserted; integer(c_int32_t),intent(out),optional::status
    character(c_char),allocatable,target::bytes(:); integer(c_int64_t)::mapped; integer(c_int8_t)::present_value,out
    integer(c_int32_t)::code
    mapped=0; if(present(value))mapped=value; present_value=0; if(present(value))present_value=1
    if(present(has_value))present_value=merge(1_c_int8_t,0_c_int8_t,has_value)
    bytes=string_bytes(term); code=c_put_text(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),mapped,present_value,out)
    inserted=out/=0; if(present(status))status=code
  end subroutine
  subroutine remove_text(self,term,removed,status)
    class(dictionary),intent(inout)::self; character(len=*),intent(in)::term; logical,intent(out)::removed
    integer(c_int32_t),intent(out),optional::status; character(c_char),allocatable,target::bytes(:)
    integer(c_int8_t)::out; integer(c_int32_t)::code
    bytes=string_bytes(term); code=c_remove_text(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),out)
    removed=out/=0; if(present(status))status=code
  end subroutine
  function contains_text(self,term,status) result(found)
    class(dictionary),intent(in)::self; character(len=*),intent(in)::term; integer(c_int32_t),intent(out),optional::status
    logical::found; character(c_char),allocatable,target::bytes(:); integer(c_int8_t)::out; integer(c_int32_t)::code
    bytes=string_bytes(term); code=c_contains_text(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),out)
    found=out/=0; if(present(status))status=code
  end function
  subroutine get_text(self,term,found,value,has_value,status)
    class(dictionary),intent(in)::self; character(len=*),intent(in)::term; logical,intent(out)::found,has_value
    integer(c_int64_t),intent(out)::value; integer(c_int32_t),intent(out),optional::status
    character(c_char),allocatable,target::bytes(:); integer(c_int8_t)::f,h; integer(c_int32_t)::code
    bytes=string_bytes(term); code=c_get_text(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),f,value,h)
    found=f/=0; has_value=h/=0; if(present(status))status=code
  end subroutine

  subroutine put_u64(self,term,value,has_value,inserted,status)
    class(dictionary),intent(inout)::self; integer(c_int64_t),target,intent(in)::term(:)
    integer(c_int64_t),intent(in),optional::value; logical,intent(in),optional::has_value; logical,intent(out)::inserted
    integer(c_int32_t),intent(out),optional::status; integer(c_int64_t)::mapped; integer(c_int8_t)::h,out
    integer(c_int32_t)::code
    mapped=0
    if(present(value))mapped=value
    h=0
    if(present(value))h=1
    if(present(has_value))h=merge(1_c_int8_t,0_c_int8_t,has_value)
    code=c_put_u64(self%handle,tokens_ptr(term),int(size(term),c_size_t),mapped,h,out); inserted=out/=0
    if(present(status))status=code
  end subroutine
  subroutine remove_u64(self,term,removed,status)
    class(dictionary),intent(inout)::self; integer(c_int64_t),target,intent(in)::term(:); logical,intent(out)::removed
    integer(c_int32_t),intent(out),optional::status; integer(c_int8_t)::out; integer(c_int32_t)::code
    code=c_remove_u64(self%handle,tokens_ptr(term),int(size(term),c_size_t),out); removed=out/=0; if(present(status))status=code
  end subroutine
  function contains_u64(self,term,status) result(found)
    class(dictionary),intent(in)::self
    integer(c_int64_t),target,intent(in)::term(:)
    integer(c_int32_t),intent(out),optional::status
    logical::found; integer(c_int8_t)::out; integer(c_int32_t)::code
    code=c_contains_u64(self%handle,tokens_ptr(term),int(size(term),c_size_t),out); found=out/=0; if(present(status))status=code
  end function
  subroutine get_u64(self,term,found,value,has_value,status)
    class(dictionary),intent(in)::self; integer(c_int64_t),target,intent(in)::term(:); logical,intent(out)::found,has_value
    integer(c_int64_t),intent(out)::value; integer(c_int32_t),intent(out),optional::status; integer(c_int8_t)::f,h
    integer(c_int32_t)::code
    code=c_get_u64(self%handle,tokens_ptr(term),int(size(term),c_size_t),f,value,h)
    found=f/=0; has_value=h/=0; if(present(status))status=code
  end subroutine

  subroutine put_all(self,terms,values,has_values,inserted,status)
    class(dictionary),intent(inout)::self; character(len=*),intent(in)::terms(:); integer(c_int64_t),intent(in),optional::values(:)
    logical,intent(in),optional::has_values(:); integer(c_size_t),intent(out)::inserted
    integer(c_int32_t),intent(out),optional::status; type(text_entry),allocatable,target::entries(:)
    character(c_char),allocatable,target::arena(:); type(c_ptr)::pointer; integer(c_int32_t)::code
    call make_entries(terms,values,has_values,entries,arena)
    if(size(entries)==0)then;pointer=c_null_ptr;else;pointer=c_loc(entries(1));end if
    code=c_batch(self%handle,pointer,int(size(entries),c_size_t),inserted);if(present(status))status=code
  end subroutine
  subroutine dictionary_clear(self,status)
    class(dictionary),intent(inout)::self;integer(c_int32_t),intent(out),optional::status;integer(c_int32_t)::code
    code=c_clear(self%handle);if(present(status))status=code
  end subroutine
  function dictionary_compact(self,status) result(value)
    class(dictionary),intent(inout)::self;integer(c_int32_t),intent(out),optional::status;integer(c_size_t)::value
    integer(c_int32_t)::code;code=c_compact(self%handle,value);if(present(status))status=code
  end function
  subroutine dictionary_checkpoint(self,status)
    class(dictionary),intent(inout)::self;integer(c_int32_t),intent(out),optional::status;integer(c_int32_t)::code
    code=c_checkpoint(self%handle);if(present(status))status=code
  end subroutine
  function contains_substring(self,term,status) result(found)
    class(dictionary),intent(in)::self;character(len=*),intent(in)::term;integer(c_int32_t),intent(out),optional::status
    logical::found;character(c_char),allocatable,target::bytes(:);integer(c_int8_t)::out;integer(c_int32_t)::code
    bytes=string_bytes(term);code=c_substring(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),out)
    found=out/=0;if(present(status))status=code
  end function
  function substring_frequency(self,term,status) result(value)
    class(dictionary),intent(in)::self;character(len=*),intent(in)::term;integer(c_int32_t),intent(out),optional::status
    integer(c_size_t)::value;character(c_char),allocatable,target::bytes(:);integer(c_int32_t)::code
    bytes=string_bytes(term);code=c_frequency(self%handle,chars_ptr(bytes),int(size(bytes),c_size_t),value)
    if(present(status))status=code
  end function
  subroutine vocabulary_term(self,index,term,found,status)
    class(dictionary),intent(in)::self;integer(c_int64_t),intent(in)::index
    character(kind=c_char,len=:),allocatable,intent(out)::term;logical,intent(out)::found
    integer(c_int32_t),intent(out),optional::status;integer(c_size_t)::count=0,capacity=0;integer(c_int8_t)::f=0
    integer(c_int32_t)::code;character(c_char),allocatable,target::bytes(:);integer::i
    code=c_vocab_term(self%handle,index,c_null_ptr,0_c_size_t,count,f)
    if(code==ldict_ok.and.f/=0)then
      capacity=count;allocate(bytes(capacity));code=c_vocab_term(self%handle,index,chars_ptr(bytes),capacity,count,f)
      allocate(character(kind=c_char,len=int(count))::term);do i=1,int(count);term(i:i)=bytes(i);end do
    else;allocate(character(kind=c_char,len=0)::term);end if
    found=f/=0;if(present(status))status=code
  end subroutine
  subroutine dictionary_close(self)
    class(dictionary),intent(inout)::self
    if(c_associated(self%handle))call c_free(self%handle);self%handle=c_null_ptr
  end subroutine
  subroutine dictionary_finalize(self);type(dictionary),intent(inout)::self;call self%close();end subroutine
end module vinary_tree_libdictenstein
