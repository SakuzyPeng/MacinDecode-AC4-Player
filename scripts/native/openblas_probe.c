/* Exercise the numerical/complex ABI before accepting a static OpenBLAS SDK. */
#define LAPACK_COMPLEX_STRUCTURE
#define HAVE_LAPACK_CONFIG_H
#include <cblas.h>
#include <lapacke.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

static int failures;
static void check(int ok, const char* label) {
    if (!ok) { fprintf(stderr, "FAILED: %s\n", label); failures++; }
}
static double real(lapack_complex_double z) { return lapack_complex_double_real(z); }
static double imag(lapack_complex_double z) { return lapack_complex_double_imag(z); }
static lapack_complex_double z(double r, double i) { return lapack_make_complex_double(r, i); }
static lapack_complex_double add(lapack_complex_double a, lapack_complex_double b) {
    return z(real(a) + real(b), imag(a) + imag(b));
}
static lapack_complex_double mul(lapack_complex_double a, lapack_complex_double b) {
    return z(real(a)*real(b) - imag(a)*imag(b), real(a)*imag(b) + imag(a)*real(b));
}
static double error(lapack_complex_double a, lapack_complex_double b) {
    return hypot(real(a)-real(b), imag(a)-imag(b));
}

int main(void) {
    check(sizeof(blasint) == 4 && sizeof(lapack_int) == 4, "LP64 integer ABI");
    check(openblas_get_parallel() == 1, "native thread backend");
    const float a[9] = {3, 1, 2, -1, 4, 1, 2, -2, 5};
    float product[9], u[9], vt[9], singular[3], superb[2], work[9];
    cblas_sgemm(CblasRowMajor, CblasNoTrans, CblasTrans, 3, 3, 3, 1, a, 3, a, 3, 0, product, 3);
    for (int r=0;r<3;r++) for (int c=0;c<3;c++) {
        float expected=0;
        for (int k=0;k<3;k++) expected+=a[r*3+k]*a[c*3+k];
        check(fabsf(product[r*3+c]-expected)<1e-4f, "real GEMM");
    }
    memcpy(work,a,sizeof(a));
    check(LAPACKE_sgesvd(LAPACK_ROW_MAJOR,'A','A',3,3,work,3,singular,u,3,vt,3,superb)==0, "real SVD");
    for (int r=0;r<3;r++) for (int c=0;c<3;c++) {
        float reconstructed=0;
        for (int k=0;k<3;k++) reconstructed+=u[r*3+k]*singular[k]*vt[k*3+c];
        check(fabsf(reconstructed-a[r*3+c])<1e-4f, "real SVD residual");
    }
    const double symmetric[9]={6,1,2, 1,5,-1, 2,-1,7};
    double matrix[9], rhs[3]={10,-12,25}, values[3];
    lapack_int pivots[3];
    memcpy(matrix,symmetric,sizeof(matrix));
    check(LAPACKE_dgesv(LAPACK_ROW_MAJOR,3,1,matrix,3,pivots,rhs,1)==0, "real solve");
    check(fabs(rhs[0]-1)<1e-10 && fabs(rhs[1]+2)<1e-10 && fabs(rhs[2]-3)<1e-10, "real solve residual");
    memcpy(matrix,symmetric,sizeof(matrix));
    check(LAPACKE_dsyev(LAPACK_ROW_MAJOR,'V','U',3,matrix,3,values)==0, "real eigen decomposition");
    for(int r=0;r<3;r++) for(int c=0;c<3;c++) {
        double av=0;
        for(int k=0;k<3;k++) av+=symmetric[r*3+k]*matrix[k*3+c];
        check(fabs(av-values[c]*matrix[r*3+c])<1e-10, "real eigen residual");
    }
    lapack_complex_double complex[9]={z(6,0),z(1,2),z(2,-1), z(1,-2),z(5,0),z(-1,1), z(2,1),z(-1,-1),z(7,0)};
    lapack_complex_double cx[3]={z(1,2),z(-2,1),z(3,-1)}, cb[3]={z(0,0),z(0,0),z(0,0)}, copy[9], cu[9], cvt[9];
    lapack_complex_double one=z(1,0),zero=z(0,0),cp[9];
    cblas_zgemm(CblasRowMajor,CblasNoTrans,CblasConjTrans,3,3,3,&one,complex,3,complex,3,&zero,cp,3);
    for(int r=0;r<3;r++) for(int c=0;c<3;c++) {
        lapack_complex_double sum=z(0,0);
        for(int k=0;k<3;k++) sum=add(sum,mul(complex[r*3+k],z(real(complex[c*3+k]),-imag(complex[c*3+k]))));
        check(error(sum,cp[r*3+c])<1e-10, "complex GEMM residual");
    }
    for(int r=0;r<3;r++) for(int k=0;k<3;k++) cb[r]=add(cb[r],mul(complex[r*3+k],cx[k]));
    memcpy(copy,complex,sizeof(copy));
    check(LAPACKE_zgesv(LAPACK_ROW_MAJOR,3,1,copy,3,pivots,cb,1)==0, "complex solve");
    for(int i=0;i<3;i++) check(error(cb[i],cx[i])<1e-10, "complex solve residual");
    memcpy(copy,complex,sizeof(copy));
    check(LAPACKE_zheev(LAPACK_ROW_MAJOR,'V','U',3,copy,3,values)==0, "complex eigen decomposition");
    for(int r=0;r<3;r++) for(int c=0;c<3;c++) {
        lapack_complex_double av=z(0,0);
        for(int k=0;k<3;k++) av=add(av,mul(complex[r*3+k],copy[k*3+c]));
        check(error(av,mul(z(values[c],0),copy[r*3+c]))<1e-10, "complex eigen residual");
    }
    /* A non-Hermitian matrix also exercises LAPACK's complex dot products. */
    complex[1]=z(-2,3);
    double cs[3],sb[2];
    memcpy(copy,complex,sizeof(copy));
    check(LAPACKE_zgesvd(LAPACK_ROW_MAJOR,'A','A',3,3,copy,3,cs,cu,3,cvt,3,sb)==0, "complex SVD");
    for(int r=0;r<3;r++) for(int c=0;c<3;c++) {
        lapack_complex_double sum=z(0,0);
        for(int k=0;k<3;k++) sum=add(sum,mul(mul(cu[r*3+k],z(cs[k],0)),cvt[k*3+c]));
        check(error(sum,complex[r*3+c])<1e-10, "complex SVD residual");
    }
    lapack_complex_float cf[9], cfu[9], cfvt[9];
    for(int i=0;i<9;i++) cf[i]=lapack_make_complex_float((float)real(complex[i]),(float)imag(complex[i]));
    check(LAPACKE_cgesvd(LAPACK_ROW_MAJOR,'A','A',3,3,cf,3,singular,cfu,3,cfvt,3,superb)==0, "float complex SVD");
    for(int r=0;r<3;r++) for(int c=0;c<3;c++) {
        lapack_complex_double sum=z(0,0);
        for(int k=0;k<3;k++) {
            lapack_complex_double left=z(lapack_complex_float_real(cfu[r*3+k]),lapack_complex_float_imag(cfu[r*3+k]));
            lapack_complex_double right=z(lapack_complex_float_real(cfvt[k*3+c]),lapack_complex_float_imag(cfvt[k*3+c]));
            sum=add(sum,mul(mul(left,z(singular[k],0)),right));
        }
        check(error(sum,complex[r*3+c])<1e-4, "float complex SVD residual");
    }
    printf("{\"ok\":%s,\"config\":\"%s\",\"threads\":%d}\n",failures?"false":"true",openblas_get_config(),openblas_get_num_threads());
    return failures ? 1 : 0;
}
