program fortran_variable_viewer_target
    use iso_fortran_env, only: int64, real64
    implicit none

    type :: particle
        integer :: id
        real(real64) :: position(3)
    end type

    integer(int64) :: counter = 42
    logical :: enabled = .true.
    real(real64) :: scale = 1.25_real64
    complex(real64) :: wave = (2.0_real64, -3.0_real64)
    character(len=24) :: message = 'Hello from Fortran'
    integer, target :: shifted(-2:2) = [-20, -10, 0, 10, 20]
    integer :: matrix(-1:1, 4:5)
    integer, allocatable :: allocated(:), unallocated(:)
    integer, pointer :: associated_values(:), null_values(:) => null()
    integer, pointer :: strided_values(:)
    type(particle) :: sample

    matrix = reshape([1, 2, 3, 4, 5, 6], shape(matrix))
    allocate(allocated(-3:1))
    allocated = [5, 4, 3, 2, 1]
    associated_values => shifted
    strided_values => shifted(-2:2:2)
    sample = particle(7, [1.0_real64, 2.0_real64, 3.0_real64])

    ! Set a breakpoint on the following print to inspect initialized values.
    print *, counter, enabled, scale, wave, message
    print *, shifted, matrix, allocated, associated_values, strided_values, sample
    print *, allocated(unallocated), associated(null_values)
    counter = counter + 1
    enabled = .false.
    shifted(0) = 99
    print *, counter, enabled, shifted
end program
