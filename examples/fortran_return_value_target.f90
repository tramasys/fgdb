module return_values
    implicit none
    type pair
        integer :: x
        integer :: y
    end type
contains
    function return_pair() result(value)
        type(pair) :: value
        value = pair(7, 11)
    end function
end module

program fortran_return_value_target
    use return_values
    implicit none
    type(pair), volatile :: value
    value = return_pair()
end program
