use alga::general::{ClosedAdd, ClosedMul};
use num::{One, Zero};
use std::marker::PhantomData;
use std::ops::{Add, Mul, Range};

use allocator::Allocator;
use constraint::{AreMultipliable, DimEq, ShapeConstraint};
use storage::{Storage, StorageMut};
use {DefaultAllocator, Dim, Matrix, MatrixMN, Scalar, Vector, VectorN, U1};

pub trait CsStorage<N, R, C = U1> {
    fn shape(&self) -> (R, C);
    fn nvalues(&self) -> usize;
    unsafe fn row_index_unchecked(&self, i: usize) -> usize;
    unsafe fn get_value_unchecked(&self, i: usize) -> &N;
    fn get_value(&self, i: usize) -> &N;
    fn row_index(&self, i: usize) -> usize;
    fn column_range(&self, j: usize) -> Range<usize>;
}

pub trait CsStorageMut<N, R, C = U1>: CsStorage<N, R, C> {
    /*
    /// Sets the length of this column without initializing its values and row indices.
    ///
    /// If the given length is larger than the current one, uninitialized entries are
    /// added at the end of the column `i`. This will effectively shift all the matrix entries
    /// of the columns at indices `j` with `j > i`. Therefore this is a `O(n)` operation.
    /// This is unsafe as the row indices on newly created components may end up being out
    /// of bounds.
    unsafe fn set_column_len(&mut self, i: usize, len: usize);
    */
}

#[derive(Clone, Debug)]
pub struct CsVecStorage<N: Scalar, R: Dim, C: Dim>
where
    DefaultAllocator: Allocator<usize, C>,
{
    shape: (R, C),
    p: VectorN<usize, C>,
    i: Vec<usize>,
    vals: Vec<N>,
}

impl<N: Scalar, R: Dim, C: Dim> CsStorage<N, R, C> for CsVecStorage<N, R, C>
where
    DefaultAllocator: Allocator<usize, C>,
{
    #[inline]
    fn shape(&self) -> (R, C) {
        self.shape
    }

    #[inline]
    fn nvalues(&self) -> usize {
        self.vals.len()
    }

    #[inline]
    fn column_range(&self, j: usize) -> Range<usize> {
        let end = if j + 1 == self.p.len() {
            self.nvalues()
        } else {
            self.p[j + 1]
        };

        self.p[j]..end
    }

    #[inline]
    fn row_index(&self, i: usize) -> usize {
        self.i[i]
    }

    #[inline]
    unsafe fn row_index_unchecked(&self, i: usize) -> usize {
        *self.i.get_unchecked(i)
    }

    #[inline]
    unsafe fn get_value_unchecked(&self, i: usize) -> &N {
        self.vals.get_unchecked(i)
    }

    #[inline]
    fn get_value(&self, i: usize) -> &N {
        &self.vals[i]
    }
}

/*
pub struct CsSliceStorage<'a, N: Scalar, R: Dim, C: DimAdd<U1>> {
    shape: (R, C),
    p: VectorSlice<usize, DimSum<C, U1>>,
    i: VectorSlice<usize, Dynamic>,
    vals: VectorSlice<N, Dynamic>,
}*/

/// A compressed sparse column matrix.
#[derive(Clone, Debug)]
pub struct CsMatrix<N: Scalar, R: Dim, C: Dim, S: CsStorage<N, R, C> = CsVecStorage<N, R, C>> {
    pub data: S,
    _phantoms: PhantomData<(N, R, C)>,
}

pub type CsVector<N, R, S = CsVecStorage<N, R, U1>> = CsMatrix<N, R, U1, S>;

impl<N: Scalar, R: Dim, C: Dim> CsMatrix<N, R, C>
where
    DefaultAllocator: Allocator<usize, C>,
{
    pub fn new_uninitialized_generic(nrows: R, ncols: C, nvals: usize) -> Self {
        let mut i = Vec::with_capacity(nvals);
        unsafe {
            i.set_len(nvals);
        }
        i.shrink_to_fit();

        let mut vals = Vec::with_capacity(nvals);
        unsafe {
            vals.set_len(nvals);
        }
        vals.shrink_to_fit();

        CsMatrix {
            data: CsVecStorage {
                shape: (nrows, ncols),
                p: unsafe { VectorN::new_uninitialized_generic(ncols, U1) },
                i,
                vals,
            },
            _phantoms: PhantomData,
        }
    }
}

fn cumsum<D: Dim>(a: &mut VectorN<usize, D>, b: &mut VectorN<usize, D>) -> usize
where
    DefaultAllocator: Allocator<usize, D>,
{
    assert!(a.len() == b.len());
    let mut sum = 0;

    for i in 0..a.len() {
        b[i] = sum;
        sum += a[i];
        a[i] = b[i];
    }

    sum
}

impl<N: Scalar, R: Dim, C: Dim, S: CsStorage<N, R, C>> CsMatrix<N, R, C, S> {
    pub fn nvalues(&self) -> usize {
        self.data.nvalues()
    }

    pub fn transpose(&self) -> CsMatrix<N, C, R>
    where
        DefaultAllocator: Allocator<usize, R>,
    {
        let (nrows, ncols) = self.data.shape();

        let nvals = self.nvalues();
        let mut res = CsMatrix::new_uninitialized_generic(ncols, nrows, nvals);
        let mut workspace = Vector::zeros_generic(nrows, U1);

        // Compute p.
        for i in 0..nvals {
            let row_id = self.data.row_index(i);
            workspace[row_id] += 1;
        }

        let _ = cumsum(&mut workspace, &mut res.data.p);

        // Fill the result.
        for j in 0..ncols.value() {
            let column_idx = self.data.column_range(j);

            for vi in column_idx {
                let row_id = self.data.row_index(vi);
                let shift = workspace[row_id];

                res.data.vals[shift] = *self.data.get_value(vi);
                res.data.i[shift] = j;
                workspace[row_id] += 1;
            }
        }

        res
    }

    fn scatter<R2: Dim, C2: Dim>(
        &self,
        j: usize,
        beta: N,
        timestamps: &mut [usize],
        timestamp: usize,
        workspace: &mut [N],
        mut nz: usize,
        res: &mut CsMatrix<N, R2, C2>,
    ) -> usize
    where
        N: ClosedAdd + ClosedMul,
        DefaultAllocator: Allocator<usize, C2>,
    {
        let column_idx = self.data.column_range(j);

        for vi in column_idx {
            let i = self.data.row_index(vi);
            let val = beta * *self.data.get_value(vi);

            if timestamps[i] < timestamp {
                timestamps[i] = timestamp;
                res.data.i[nz] = i;
                nz += 1;
                workspace[i] = val;
            } else {
                workspace[i] += val;
            }
        }

        nz
    }
}

/*
impl<N: Scalar, R, S> CsVector<N, R, S> {
    pub fn axpy(&mut self, alpha: N, x: CsVector<N, R, S>, beta: N) {
        // First, compute the number of non-zero entries.
        let mut nnzero = 0;

        // Allocate a size large enough.
        self.data.set_column_len(0, nnzero);

        // Fill with the axpy.
        let mut i = self.nvalues();
        let mut j = x.nvalues();
        let mut k = nnzero - 1;
        let mut rid1 = self.data.row_index(0, i - 1);
        let mut rid2 = x.data.row_index(0, j - 1);

        while k > 0 {
            if rid1 == rid2 {
                self.data.set_row_index(0, k, rid1);
                self[k] = alpha * x[j] + beta * self[k];
                i -= 1;
                j -= 1;
            } else if rid1 < rid2 {
                self.data.set_row_index(0, k, rid1);
                self[k] = beta * self[i];
                i -= 1;
            } else {
                self.data.set_row_index(0, k, rid2);
                self[k] = alpha * x[j];
                j -= 1;
            }

            k -= 1;
        }
    }
}
*/

impl<N: Scalar + Zero + ClosedAdd + ClosedMul, D: Dim, S: StorageMut<N, D>> Vector<N, D, S> {
    pub fn axpy_cs<D2: Dim, S2>(&mut self, alpha: N, x: &CsVector<N, D2, S2>, beta: N)
    where
        S2: CsStorage<N, D2>,
        ShapeConstraint: DimEq<D, D2>,
    {
        if beta.is_zero() {
            for i in 0..x.nvalues() {
                unsafe {
                    let k = x.data.row_index_unchecked(i);
                    let y = self.vget_unchecked_mut(k);
                    *y = alpha * *x.data.get_value_unchecked(i);
                }
            }
        } else {
            // Needed to be sure even components not present on `x` are multiplied.
            *self *= beta;

            for i in 0..x.nvalues() {
                unsafe {
                    let k = x.data.row_index_unchecked(i);
                    let y = self.vget_unchecked_mut(k);
                    *y += alpha * *x.data.get_value_unchecked(i);
                }
            }
        }
    }

    /*
    pub fn gemv_sparse<R2: Dim, C2: Dim, S2>(&mut self, alpha: N, a: &CsMatrix<N, R2, C2, S2>, x: &DVector<N>, beta: N)
        where
            S2: CsStorage<N, R2, C2> {
        let col2 = a.column(0);
        let val = unsafe { *x.vget_unchecked(0) };
        self.axpy_sparse(alpha * val, &col2, beta);
    
        for j in 1..ncols2 {
            let col2 = a.column(j);
            let val = unsafe { *x.vget_unchecked(j) };
    
            self.axpy_sparse(alpha * val, &col2, N::one());
        }
    }
    */
}

impl<'a, 'b, N, R1, R2, C1, C2, S1, S2> Mul<&'b CsMatrix<N, R2, C2, S2>>
    for &'a CsMatrix<N, R1, C1, S1>
where
    N: Scalar + ClosedAdd + ClosedMul + Zero,
    R1: Dim,
    C1: Dim,
    R2: Dim,
    C2: Dim,
    S1: CsStorage<N, R1, C1>,
    S2: CsStorage<N, R2, C2>,
    ShapeConstraint: AreMultipliable<R1, C1, R2, C2>,
    DefaultAllocator: Allocator<usize, C2> + Allocator<usize, R1> + Allocator<N, R1>,
{
    type Output = CsMatrix<N, R1, C2>;

    fn mul(self, rhs: &'b CsMatrix<N, R2, C2, S2>) -> CsMatrix<N, R1, C2> {
        let (nrows1, ncols1) = self.data.shape();
        let (nrows2, ncols2) = rhs.data.shape();
        assert_eq!(
            ncols1.value(),
            nrows2.value(),
            "Mismatched dimensions for matrix multiplication."
        );

        let mut res =
            CsMatrix::new_uninitialized_generic(nrows1, ncols2, self.nvalues() + rhs.nvalues());
        let mut timestamps = VectorN::zeros_generic(nrows1, U1);
        let mut workspace = unsafe { VectorN::new_uninitialized_generic(nrows1, U1) };
        let mut nz = 0;

        for j in 0..ncols2.value() {
            res.data.p[j] = nz;
            let column_idx = rhs.data.column_range(j);
            let new_size_bound = nz + nrows1.value();
            res.data.i.resize(new_size_bound, 0);
            res.data.vals.resize(new_size_bound, N::zero());

            for vi in column_idx {
                let i = rhs.data.row_index(vi);
                nz = self.scatter(
                    i,
                    *rhs.data.get_value(vi),
                    timestamps.as_mut_slice(),
                    j + 1,
                    workspace.as_mut_slice(),
                    nz,
                    &mut res,
                );
            }

            for p in res.data.p[j]..nz {
                res.data.vals[p] = workspace[res.data.i[p]]
            }
        }

        res.data.i.truncate(nz);
        res.data.i.shrink_to_fit();
        res.data.vals.truncate(nz);
        res.data.vals.shrink_to_fit();
        res
    }
}

impl<'a, 'b, N, R1, R2, C1, C2, S1, S2> Add<&'b CsMatrix<N, R2, C2, S2>>
    for &'a CsMatrix<N, R1, C1, S1>
where
    N: Scalar + ClosedAdd + ClosedMul + One,
    R1: Dim,
    C1: Dim,
    R2: Dim,
    C2: Dim,
    S1: CsStorage<N, R1, C1>,
    S2: CsStorage<N, R2, C2>,
    ShapeConstraint: DimEq<R1, R2> + DimEq<C1, C2>,
    DefaultAllocator: Allocator<usize, C2> + Allocator<usize, R1> + Allocator<N, R1>,
{
    type Output = CsMatrix<N, R1, C2>;

    fn add(self, rhs: &'b CsMatrix<N, R2, C2, S2>) -> CsMatrix<N, R1, C2> {
        let (nrows1, ncols1) = self.data.shape();
        let (nrows2, ncols2) = rhs.data.shape();
        assert_eq!(
            (nrows1.value(), ncols1.value()),
            (nrows2.value(), ncols2.value()),
            "Mismatched dimensions for matrix sum."
        );

        let mut res =
            CsMatrix::new_uninitialized_generic(nrows1, ncols2, self.nvalues() + rhs.nvalues());
        let mut timestamps = VectorN::zeros_generic(nrows1, U1);
        let mut workspace = unsafe { VectorN::new_uninitialized_generic(nrows1, U1) };
        let mut nz = 0;

        for j in 0..ncols2.value() {
            res.data.p[j] = nz;

            nz = self.scatter(
                j,
                N::one(),
                timestamps.as_mut_slice(),
                j + 1,
                workspace.as_mut_slice(),
                nz,
                &mut res,
            );

            nz = rhs.scatter(
                j,
                N::one(),
                timestamps.as_mut_slice(),
                j + 1,
                workspace.as_mut_slice(),
                nz,
                &mut res,
            );

            for p in res.data.p[j]..nz {
                res.data.vals[p] = workspace[res.data.i[p]]
            }
        }

        res.data.i.truncate(nz);
        res.data.i.shrink_to_fit();
        res.data.vals.truncate(nz);
        res.data.vals.shrink_to_fit();
        res
    }
}

impl<'a, N: Scalar + Zero, R: Dim, C: Dim, S> From<CsMatrix<N, R, C, S>> for MatrixMN<N, R, C>
where
    S: CsStorage<N, R, C>,
    DefaultAllocator: Allocator<N, R, C>,
{
    fn from(m: CsMatrix<N, R, C, S>) -> Self {
        let (nrows, ncols) = m.data.shape();
        let mut res = MatrixMN::zeros_generic(nrows, ncols);

        for j in 0..ncols.value() {
            let column_idx = m.data.column_range(j);

            for iv in column_idx {
                let i = m.data.row_index(iv);
                res[(i, j)] = *m.data.get_value(iv);
            }
        }

        res
    }
}

impl<'a, N: Scalar + Zero, R: Dim, C: Dim, S> From<Matrix<N, R, C, S>> for CsMatrix<N, R, C>
where
    S: Storage<N, R, C>,
    DefaultAllocator: Allocator<N, R, C> + Allocator<usize, C>,
{
    fn from(m: Matrix<N, R, C, S>) -> Self {
        let (nrows, ncols) = m.data.shape();
        let nvalues = m.iter().filter(|e| !e.is_zero()).count();
        let mut res = CsMatrix::new_uninitialized_generic(nrows, ncols, nvalues);
        let mut nz = 0;

        for j in 0..ncols.value() {
            let column = m.column(j);
            res.data.p[j] = nz;

            for i in 0..nrows.value() {
                if !column[i].is_zero() {
                    res.data.i[nz] = i;
                    res.data.vals[nz] = column[i];
                    nz += 1;
                }
            }
        }

        res
    }
}