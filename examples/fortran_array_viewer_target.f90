program fortran_array_viewer_target
    implicit none
    integer :: i, j, k
    integer, target :: matrix(-20:79, 4:103)
    integer :: cube(-1:2, 4:8, -3:2)
    integer, allocatable :: large(:)
    integer, pointer :: reversed(:,:), strided(:,:)
    integer, pointer :: missing(:) => null()

    allocate(large(-100:99899))

    do i = -100, 99899
        large(i) = i * 3
    end do

    do j = 4, 103
        do i = -20, 79
            matrix(i, j) = i * 1000 + j
        end do
    end do

    do k = -3, 2
        do j = 4, 8
            do i = -1, 2
                cube(i, j, k) = i * 100 + j * 10 + k
            end do
        end do
    end do

    reversed => matrix(79:-20:-1, 103:4:-1)
    strided => matrix(-20:79:3, 4:103:2)

    ! Break at fortran_arrays_ready, then select caller frame 1.
    call fortran_arrays_ready()
    print *, large(99000), matrix(79, 103), cube(2, 8, 2)
    print *, reversed(1, 1), strided(2, 3), associated(missing)
end program

subroutine fortran_arrays_ready()
    implicit none
    integer, volatile :: ready
    ready = 1
end subroutine
